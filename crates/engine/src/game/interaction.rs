//! Hidden engine-authority interaction projection and submission boundary.
//!
//! No production transport calls this module yet. The existing human action UI
//! remains the only exposed authority until the separately reviewed adapter
//! cutover. Engine tests may use these entry points to prove the contract.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::ai_support::{
    validated_candidate_actions_for_semantic_owner, ActionMetadata, CandidateAction,
    FilterPipeline, TacticalClass,
};
use crate::analysis::decision_template::{
    predictability_gate, validate_pins, DecisionGroupKey, DecisionKind, DecisionTemplate,
    IterationCount, PinnedDecision, ReplayMode, TargetPin,
};
use crate::types::ability::{
    AggregateFunction, ChoiceType, ChooseFromZoneConstraint, Comparator, CounterCostSelection,
    DoorLockOp, EffectKind, ObjectProperty, SearchSelectionConstraint, TapCreaturesAggregateStat,
    TargetRef,
};
use crate::types::actions::{
    AlternativeCastDecision, CastChoice, GameAction, MulliganChoice, OutsideGameSelection,
    PrecastCopyShortcutResponse, UnlessCostBranch,
};
use crate::types::card_type::CoreType;
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::game_state::{
    ActionResult, AutoMayChoice, CastPaymentMode, CastingVariant, CombatDamageAssignmentMode,
    ConvokeMode, CounterCostChoice, CounterMoveChoice, CounterRemoveChoice, GameState, ManaChoice,
    ManaChoiceContext, ManaChoicePrompt, OutsideGameChoiceSource, PayCostKind, PileSide,
    ShardChoice, ShardOptions, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::interaction::{
    ActiveInteractionSlot, AggregateComparator, AmountAssignment, ConfirmSemantics,
    InteractionActionCode, InteractionAggregateFunction, InteractionAvailability,
    InteractionChoice, InteractionChoiceId, InteractionChoiceStatus,
    InteractionDamageAssignmentMode, InteractionGroupConstraint, InteractionId,
    InteractionIntentCode, InteractionManaColor, InteractionObjectProperty, InteractionOpportunity,
    InteractionOpportunityResponse, InteractionOutcomeCode, InteractionPresentationSurface,
    InteractionPreview, InteractionPreviewRequest, InteractionPreviewStatus, InteractionProgress,
    InteractionReasonCode, InteractionRelationConstraint, InteractionRelationSourceConstraint,
    InteractionResponse, InteractionResponseSpec, InteractionRoleCode, InteractionSessionId,
    InteractionShortcutCountSpec, InteractionShortcutDecision, InteractionShortcutPoint,
    InteractionShortcutPointKind, InteractionShortcutReply, InteractionShortcutResponseCode,
    InteractionSlotKind, InteractionSubmission, InteractionSummaryCode, InteractionWaitingForCode,
    InteractionWaitingForKind, InteractionZoneCode, SelectionConstraint, SimultaneousDecisionKind,
    ViewerInteraction, MAX_INTERACTION_LIST_LEN,
};
use crate::types::mana::{ManaColor, ManaCost, ManaType};
use crate::types::match_config::DeckCardCount;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::combat::AttackTarget;
use super::dungeon::DungeonId;
use super::engine::{
    apply_interaction, apply_interaction_for_simulation, EngineError, MAX_SHORTCUT_CYCLES,
};
use super::game_object::RoomDoor;
use super::merge::MergeSide;
use super::{turn_control, visibility};

pub const MAX_INTERACTION_STRING_LEN: usize = 256;
const MAX_INTERACTION_SESSION_ID_LEN: usize = 128;
const MAX_INTERACTION_SERIAL_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WaitingClassification {
    code: InteractionWaitingForCode,
    simultaneous: Option<SimultaneousDecisionKind>,
    slot_kind: Option<InteractionSlotKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumberResponseAction {
    ChooseX,
    PayAmount,
    AssistPayment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CombatRelationAction {
    Attackers,
    Blockers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManaGroupAction {
    PayManaAbility,
    ChooseSingleColor,
    ChooseCombination,
    ChooseAnyCombination,
    Phyrexian,
}

/// Type-level assertion that the delegated candidate family was reviewed as a complete,
/// finite one-step enumeration. Keeping this token in the classifier forces additions to the
/// exhaustive `WaitingFor` match to make the completeness claim explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AuditedExactCandidates;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HumanResponseModel {
    Terminal,
    ExactCandidates(AuditedExactCandidates),
    Select,
    AssignAmounts,
    AmountAssignments,
    DamageAssignments,
    TriggerOrder,
    CoinFlipSequence,
    TargetSequence,
    CategorySelection,
    CombatRelations(CombatRelationAction),
    ManaGroups(ManaGroupAction),
    ModeSequence,
    OutsideSelection,
    TextChoice,
    ShortcutReply,
    DirectChoices,
    SideboardPartition,
    NumberRange(NumberResponseAction),
    LoopShortcut,
}

/// Exhaustive authority boundary for human responses. Only variants in
/// `ExactCandidates` may consult the AI candidate generator; every family
/// whose AI producer intentionally prunes its search space is either projected
/// as a complete schema or fails closed until its complete schema exists.
fn human_response_model(waiting_for: &WaitingFor, semantic_owner: PlayerId) -> HumanResponseModel {
    match waiting_for {
        WaitingFor::GameOver { .. } => HumanResponseModel::Terminal,
        WaitingFor::OrderTriggers { .. } => HumanResponseModel::TriggerOrder,
        WaitingFor::CoinFlipKeepChoice { .. } => HumanResponseModel::CoinFlipSequence,
        WaitingFor::ChooseXValue { .. } => {
            HumanResponseModel::NumberRange(NumberResponseAction::ChooseX)
        }
        WaitingFor::PayAmountChoice { .. } => {
            HumanResponseModel::NumberRange(NumberResponseAction::PayAmount)
        }
        WaitingFor::AssistPayment { .. } => {
            HumanResponseModel::NumberRange(NumberResponseAction::AssistPayment)
        }
        WaitingFor::PayCost {
            kind:
                PayCostKind::RemoveCounter {
                    selection: CounterCostSelection::AmongObjects,
                    ..
                },
            ..
        } => HumanResponseModel::AssignAmounts,
        WaitingFor::AssignCombatDamage { .. } => HumanResponseModel::DamageAssignments,
        WaitingFor::AssignBlockerDamage { .. }
        | WaitingFor::DistributeAmong { .. }
        | WaitingFor::MoveCountersDistribution { .. }
        | WaitingFor::RemoveCountersChoice { .. } => HumanResponseModel::AmountAssignments,
        WaitingFor::OpeningHandBottomCards { .. } => HumanResponseModel::Select,
        WaitingFor::MulliganDecision { pending, .. }
            if pending.iter().any(|entry| {
                entry.player == semantic_owner
                    && matches!(
                        entry.phase,
                        crate::types::game_state::MulliganDecisionPhase::BottomCards { .. }
                    )
            }) =>
        {
            HumanResponseModel::Select
        }
        WaitingFor::TargetSelection { .. }
        | WaitingFor::TriggerTargetSelection { .. }
        | WaitingFor::MultiTargetSelection { .. }
        | WaitingFor::ChooseObjectsSelection { .. }
        | WaitingFor::EachPlayerCopyChosenSelection { .. }
        | WaitingFor::ProliferateChoice { .. }
        | WaitingFor::TimeTravelChoice { .. }
        | WaitingFor::RetargetChoice { .. } => HumanResponseModel::TargetSequence,
        WaitingFor::CategoryChoice { .. } => HumanResponseModel::CategorySelection,
        WaitingFor::ChooseUntapSubset { .. }
        | WaitingFor::CrewVehicle { .. }
        | WaitingFor::StationTarget { .. }
        | WaitingFor::SaddleMount { .. }
        | WaitingFor::EffectZoneChoice { .. }
        | WaitingFor::WardSacrificeChoice { .. }
        | WaitingFor::UnlessBounceChoice { .. }
        | WaitingFor::ChooseRingBearer { .. }
        | WaitingFor::PayCost { .. }
        | WaitingFor::BlightChoice { .. }
        | WaitingFor::HarmonizeTapChoice { .. }
        | WaitingFor::KeepWithinTotalPowerChoice { .. }
        | WaitingFor::KeepExactPermanentsChoice { .. }
        | WaitingFor::ScryChoice { .. }
        | WaitingFor::ArrangePlanarDeckTopChoice { .. }
        | WaitingFor::DigChoice { .. }
        | WaitingFor::SurveilChoice { .. }
        | WaitingFor::SearchChoice { .. }
        | WaitingFor::SearchPartitionChoice { .. }
        | WaitingFor::ChooseFromZoneChoice { .. }
        | WaitingFor::ConniveDiscard { .. }
        | WaitingFor::DrawnThisTurnTopdeckChoice { .. }
        | WaitingFor::DiscardToHandSize { .. }
        | WaitingFor::WardDiscardChoice { .. }
        | WaitingFor::CollectEvidenceChoice { .. }
        | WaitingFor::SeparatePilesPartition { .. }
        | WaitingFor::DiscardChoice { .. } => HumanResponseModel::Select,
        WaitingFor::DeclareAttackers { .. } => {
            HumanResponseModel::CombatRelations(CombatRelationAction::Attackers)
        }
        WaitingFor::DeclareBlockers { .. } => {
            HumanResponseModel::CombatRelations(CombatRelationAction::Blockers)
        }
        WaitingFor::PayManaAbilityMana { .. } => {
            HumanResponseModel::ManaGroups(ManaGroupAction::PayManaAbility)
        }
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { .. },
            ..
        } => HumanResponseModel::ManaGroups(ManaGroupAction::ChooseSingleColor),
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::Combination { .. },
            ..
        } => HumanResponseModel::ManaGroups(ManaGroupAction::ChooseCombination),
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::AnyCombination { .. },
            ..
        } => HumanResponseModel::ManaGroups(ManaGroupAction::ChooseAnyCombination),
        WaitingFor::PhyrexianPayment { .. } => {
            HumanResponseModel::ManaGroups(ManaGroupAction::Phyrexian)
        }
        WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => {
            HumanResponseModel::ModeSequence
        }
        WaitingFor::OutsideGameChoice { .. } => HumanResponseModel::OutsideSelection,
        WaitingFor::NamedChoice { .. } => HumanResponseModel::TextChoice,
        WaitingFor::RespondToShortcut { .. } => HumanResponseModel::ShortcutReply,
        WaitingFor::PrecastCopyShortcutOffer { .. }
        | WaitingFor::RespondToPrecastCopyShortcut { .. }
        | WaitingFor::CommanderZoneChoice { .. }
        | WaitingFor::UntapChoice { .. } => HumanResponseModel::DirectChoices,
        WaitingFor::BetweenGamesSideboard { .. } => HumanResponseModel::SideboardPartition,
        WaitingFor::ManaPayment { .. } => HumanResponseModel::DirectChoices,
        WaitingFor::LoopShortcut { .. } => HumanResponseModel::LoopShortcut,
        WaitingFor::Priority { .. }
        | WaitingFor::MeldPairChoice { .. }
        | WaitingFor::MeldAttackTargetChoice { .. }
        | WaitingFor::MulliganDecision { .. }
        | WaitingFor::AssistChoosePlayer { .. }
        | WaitingFor::ExertChoice { .. }
        | WaitingFor::EnlistChoice { .. }
        | WaitingFor::ReplacementChoice { .. }
        | WaitingFor::CopyTargetChoice { .. }
        | WaitingFor::ExploreChoice { .. }
        | WaitingFor::ReturnAsAuraTarget { .. }
        | WaitingFor::EquipTarget { .. }
        | WaitingFor::RedistributeLifeTotals { .. }
        | WaitingFor::RevealChoice { .. }
        | WaitingFor::BeholdChoice { .. }
        | WaitingFor::ChooseOneOfBranch { .. }
        | WaitingFor::LearnChoice { .. }
        | WaitingFor::ManifestDreadChoice { .. }
        | WaitingFor::BetweenGamesChoosePlayDraw { .. }
        | WaitingFor::OpponentGuess { .. }
        | WaitingFor::SpellbookDraft { .. }
        | WaitingFor::DamageSourceChoice { .. }
        | WaitingFor::OptionalCostChoice { .. }
        | WaitingFor::SpliceOffer { .. }
        | WaitingFor::DefilerPayment { .. }
        | WaitingFor::CastOffer { .. }
        | WaitingFor::ModalFaceChoice { .. }
        | WaitingFor::AlternativeCastChoice { .. }
        | WaitingFor::MutateMergeChoice { .. }
        | WaitingFor::CipherEncodeChoice { .. }
        | WaitingFor::CastingVariantChoice { .. }
        | WaitingFor::ChoosePermanentTypeSlot { .. }
        | WaitingFor::OptionalEffectChoice { .. }
        | WaitingFor::PairChoice { .. }
        | WaitingFor::TributeChoice { .. }
        | WaitingFor::MiracleReveal { .. }
        | WaitingFor::OpponentMayChoice { .. }
        | WaitingFor::UnlessPayment { .. }
        | WaitingFor::UnlessPaymentChooseCost { .. }
        | WaitingFor::ChooseRoomDoor { .. }
        | WaitingFor::ChooseDungeon { .. }
        | WaitingFor::ChooseDungeonRoom { .. }
        | WaitingFor::SpecializeColor { .. }
        | WaitingFor::ActivationCostOneOfChoice { .. }
        | WaitingFor::CostTypeChoice { .. }
        | WaitingFor::RevealUntilKeptChoice { .. }
        | WaitingFor::RepeatDecision { .. }
        | WaitingFor::TopOrBottomChoice { .. }
        | WaitingFor::PopulateChoice { .. }
        | WaitingFor::ClashChooseOpponent { .. }
        | WaitingFor::ChooseFromZoneOpponentChooser { .. }
        | WaitingFor::ChooseAnnouncingOpponent { .. }
        | WaitingFor::ClashCardPlacement { .. }
        | WaitingFor::VoteChoice { .. }
        | WaitingFor::SeparatePilesChooseOpponent { .. }
        | WaitingFor::SeparatePilesChoice { .. }
        | WaitingFor::CompanionReveal { .. }
        | WaitingFor::ChooseLegend { .. }
        | WaitingFor::BattleProtectorChoice { .. }
        | WaitingFor::CopyRetarget { .. }
        | WaitingFor::CombatTaxPayment { .. } => {
            HumanResponseModel::ExactCandidates(AuditedExactCandidates)
        }
    }
}

/// Exhaustive classification of every current `WaitingFor` variant. Protocol
/// consumers see a stable semantic code plus simultaneous-slot metadata; the
/// opportunity response variant remains the response-shape authority.
fn classify_waiting_for(waiting_for: &WaitingFor) -> WaitingClassification {
    let (code, simultaneous, slot_kind) = match waiting_for {
        WaitingFor::GameOver { .. } => (InteractionWaitingForCode::Terminal, None, None),
        WaitingFor::MulliganDecision { .. } => (
            InteractionWaitingForCode::Mulligan,
            Some(SimultaneousDecisionKind::Mulligan),
            Some(InteractionSlotKind::Mulligan),
        ),
        WaitingFor::OpeningHandBottomCards { .. } => (
            InteractionWaitingForCode::OpeningBottom,
            Some(SimultaneousDecisionKind::OpeningBottom),
            Some(InteractionSlotKind::OpeningBottom),
        ),
        WaitingFor::ManaPayment { .. }
        | WaitingFor::AssistPayment { .. }
        | WaitingFor::DefilerPayment { .. }
        | WaitingFor::UnlessPayment { .. }
        | WaitingFor::CombatTaxPayment { .. } => (
            InteractionWaitingForCode::Choose,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::PayManaAbilityMana { .. }
        | WaitingFor::ChooseManaColor { .. }
        | WaitingFor::PhyrexianPayment { .. } => (
            InteractionWaitingForCode::ManaGroups,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::CategoryChoice { .. } => (
            InteractionWaitingForCode::Sequence,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::DeclareAttackers { .. } | WaitingFor::DeclareBlockers { .. } => (
            InteractionWaitingForCode::Relations,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::OrderTriggers { .. } => (
            InteractionWaitingForCode::Sequence,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::CoinFlipKeepChoice { .. } => (
            InteractionWaitingForCode::Sequence,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => (
            InteractionWaitingForCode::Sequence,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::OutsideGameChoice { .. } => (
            InteractionWaitingForCode::Select,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::NamedChoice { .. } => (
            InteractionWaitingForCode::Text,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::BetweenGamesSideboard { .. } => (
            InteractionWaitingForCode::DeckPartition,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::TargetSelection { .. }
        | WaitingFor::TriggerTargetSelection { .. }
        | WaitingFor::MultiTargetSelection { .. }
        | WaitingFor::ChooseObjectsSelection { .. }
        | WaitingFor::EachPlayerCopyChosenSelection { .. }
        | WaitingFor::ProliferateChoice { .. }
        | WaitingFor::TimeTravelChoice { .. }
        | WaitingFor::RetargetChoice { .. } => (
            InteractionWaitingForCode::Sequence,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::RedistributeLifeTotals { .. } => (
            InteractionWaitingForCode::Choose,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::AssignCombatDamage { .. } => (
            InteractionWaitingForCode::AssignDamage,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::AssignBlockerDamage { .. }
        | WaitingFor::DistributeAmong { .. }
        | WaitingFor::MoveCountersDistribution { .. }
        | WaitingFor::RemoveCountersChoice { .. } => (
            InteractionWaitingForCode::AssignAmounts,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::PayAmountChoice { .. } => (
            InteractionWaitingForCode::Number,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::LoopShortcut { .. } | WaitingFor::RespondToShortcut { .. } => (
            InteractionWaitingForCode::Shortcut,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::PayCost {
            kind:
                PayCostKind::RemoveCounter {
                    selection: CounterCostSelection::AmongObjects,
                    ..
                },
            ..
        } => (
            InteractionWaitingForCode::AssignAmounts,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::ChooseUntapSubset { .. }
        | WaitingFor::CrewVehicle { .. }
        | WaitingFor::StationTarget { .. }
        | WaitingFor::SaddleMount { .. }
        | WaitingFor::EffectZoneChoice { .. }
        | WaitingFor::WardSacrificeChoice { .. }
        | WaitingFor::UnlessBounceChoice { .. }
        | WaitingFor::ChooseRingBearer { .. }
        | WaitingFor::PayCost { .. }
        | WaitingFor::BlightChoice { .. }
        | WaitingFor::HarmonizeTapChoice { .. }
        | WaitingFor::KeepWithinTotalPowerChoice { .. }
        | WaitingFor::KeepExactPermanentsChoice { .. }
        | WaitingFor::ScryChoice { .. }
        | WaitingFor::ArrangePlanarDeckTopChoice { .. }
        | WaitingFor::DigChoice { .. }
        | WaitingFor::SurveilChoice { .. }
        | WaitingFor::SearchChoice { .. }
        | WaitingFor::SearchPartitionChoice { .. }
        | WaitingFor::ChooseFromZoneChoice { .. }
        | WaitingFor::ConniveDiscard { .. }
        | WaitingFor::DiscardChoice {
            unless_filter: None,
            ..
        }
        | WaitingFor::DrawnThisTurnTopdeckChoice { .. }
        | WaitingFor::DiscardToHandSize { .. }
        | WaitingFor::WardDiscardChoice { .. }
        | WaitingFor::CollectEvidenceChoice { .. }
        | WaitingFor::SeparatePilesPartition { .. } => (
            InteractionWaitingForCode::Select,
            None,
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::Priority { .. }
        | WaitingFor::MeldPairChoice { .. }
        | WaitingFor::MeldAttackTargetChoice { .. }
        | WaitingFor::ChooseXValue { .. }
        | WaitingFor::UntapChoice { .. }
        | WaitingFor::ExertChoice { .. }
        | WaitingFor::EnlistChoice { .. }
        | WaitingFor::ReplacementChoice { .. }
        | WaitingFor::CopyTargetChoice { .. }
        | WaitingFor::ExploreChoice { .. }
        | WaitingFor::ReturnAsAuraTarget { .. }
        | WaitingFor::EquipTarget { .. }
        | WaitingFor::RevealChoice { .. }
        | WaitingFor::BeholdChoice { .. }
        | WaitingFor::DiscardChoice {
            unless_filter: Some(_),
            ..
        }
        | WaitingFor::ChooseOneOfBranch { .. }
        | WaitingFor::LearnChoice { .. }
        | WaitingFor::ManifestDreadChoice { .. }
        | WaitingFor::BetweenGamesChoosePlayDraw { .. }
        | WaitingFor::OpponentGuess { .. }
        | WaitingFor::SpellbookDraft { .. }
        | WaitingFor::DamageSourceChoice { .. }
        | WaitingFor::OptionalCostChoice { .. }
        | WaitingFor::SpliceOffer { .. }
        | WaitingFor::CastOffer { .. }
        | WaitingFor::ModalFaceChoice { .. }
        | WaitingFor::AlternativeCastChoice { .. }
        | WaitingFor::MutateMergeChoice { .. }
        | WaitingFor::CipherEncodeChoice { .. }
        | WaitingFor::CastingVariantChoice { .. }
        | WaitingFor::ChoosePermanentTypeSlot { .. }
        | WaitingFor::OptionalEffectChoice { .. }
        | WaitingFor::PairChoice { .. }
        | WaitingFor::TributeChoice { .. }
        | WaitingFor::MiracleReveal { .. }
        | WaitingFor::OpponentMayChoice { .. }
        | WaitingFor::PrecastCopyShortcutOffer { .. }
        | WaitingFor::RespondToPrecastCopyShortcut { .. }
        | WaitingFor::UnlessPaymentChooseCost { .. }
        | WaitingFor::ChooseRoomDoor { .. }
        | WaitingFor::ChooseDungeon { .. }
        | WaitingFor::ChooseDungeonRoom { .. }
        | WaitingFor::SpecializeColor { .. }
        | WaitingFor::ActivationCostOneOfChoice { .. }
        | WaitingFor::CostTypeChoice { .. }
        | WaitingFor::RevealUntilKeptChoice { .. }
        | WaitingFor::RepeatDecision { .. }
        | WaitingFor::TopOrBottomChoice { .. }
        | WaitingFor::PopulateChoice { .. }
        | WaitingFor::ClashChooseOpponent { .. }
        | WaitingFor::ChooseFromZoneOpponentChooser { .. }
        | WaitingFor::ChooseAnnouncingOpponent { .. }
        | WaitingFor::ClashCardPlacement { .. }
        | WaitingFor::VoteChoice { .. }
        | WaitingFor::SeparatePilesChooseOpponent { .. }
        | WaitingFor::SeparatePilesChoice { .. }
        | WaitingFor::CompanionReveal { .. }
        | WaitingFor::ChooseLegend { .. }
        | WaitingFor::CommanderZoneChoice { .. }
        | WaitingFor::BattleProtectorChoice { .. }
        | WaitingFor::AssistChoosePlayer { .. }
        | WaitingFor::CopyRetarget { .. } => (
            InteractionWaitingForCode::Choose,
            None,
            Some(InteractionSlotKind::Single),
        ),
    };
    WaitingClassification {
        code,
        simultaneous,
        slot_kind,
    }
}

fn waiting_for_kind(waiting_for: &WaitingFor) -> InteractionWaitingForKind {
    let classification = classify_waiting_for(waiting_for);
    InteractionWaitingForKind {
        simultaneous: classification.simultaneous,
        terminal: classification.slot_kind.is_none(),
        code: classification.code,
    }
}

fn semantic_slots(waiting_for: &WaitingFor) -> Vec<(PlayerId, InteractionSlotKind)> {
    let classification = classify_waiting_for(waiting_for);
    let Some(slot_kind) = classification.slot_kind else {
        return Vec::new();
    };
    waiting_for
        .acting_players()
        .into_iter()
        .map(|player| (player, slot_kind))
        .collect()
}

fn interaction_serial_is_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_INTERACTION_SERIAL_LEN
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.bytes().any(|byte| byte != b'0')
}

fn interaction_session_is_valid(session: &InteractionSessionId) -> bool {
    !session.0.is_empty() && session.0.len() <= MAX_INTERACTION_SESSION_ID_LEN
}

fn increment_decimal(value: &str) -> Option<String> {
    if !interaction_serial_is_valid(value) {
        return None;
    }
    let mut bytes = value.as_bytes().to_vec();
    let mut carry = true;
    for byte in bytes.iter_mut().rev() {
        if !carry {
            break;
        }
        if *byte == b'9' {
            *byte = b'0';
        } else {
            *byte += 1;
            carry = false;
        }
    }
    if carry {
        if bytes.len() == MAX_INTERACTION_SERIAL_LEN {
            return None;
        }
        bytes.insert(0, b'1');
    }
    String::from_utf8(bytes).ok()
}

fn allocate_interaction_ids(
    state: &GameState,
    count: usize,
) -> Option<(Vec<InteractionId>, u64, String)> {
    if !interaction_serial_is_valid(&state.next_interaction_serial) {
        return None;
    }
    let session = state.interaction_session_id.as_ref()?;
    if !interaction_session_is_valid(session) {
        return None;
    }
    let mut generation = state.interaction_generation;
    let mut serial = state.next_interaction_serial.clone();
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        let id = InteractionId(format!("{}.{}.{}", session.0, generation, serial));
        if id.0.len() > MAX_INTERACTION_STRING_LEN {
            return None;
        }
        ids.push(id);
        if let Some(next) = increment_decimal(&serial) {
            serial = next;
        } else {
            generation = generation.checked_add(1)?;
            serial = "1".to_string();
        }
    }
    Some((ids, generation, serial))
}

fn bind_all_current_slots(state: &mut GameState) -> bool {
    let semantic = semantic_slots(&state.waiting_for);
    let Some((ids, generation, serial)) = allocate_interaction_ids(state, semantic.len()) else {
        return false;
    };
    let slots = semantic
        .into_iter()
        .zip(ids)
        .map(
            |((owner, slot_kind), interaction_id)| ActiveInteractionSlot {
                semantic_owner: owner.0,
                slot_kind,
                interaction_id,
            },
        )
        .collect();
    state.interaction_generation = generation;
    state.next_interaction_serial = serial;
    state.active_interaction_slots = slots;
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionBindError {
    pub code: InteractionReasonCode,
}

/// Bind trusted authority for a new or pre-contract deserialized state.
pub fn bind_interaction_authority(
    state: &mut GameState,
    session: InteractionSessionId,
) -> Result<(), InteractionBindError> {
    if !interaction_session_is_valid(&session) {
        state.active_interaction_slots.clear();
        return Err(InteractionBindError {
            code: InteractionReasonCode::InvalidAuthorityState,
        });
    }
    let previous_session = state.interaction_session_id.clone();
    let previous_generation = state.interaction_generation;
    let previous_serial = state.next_interaction_serial.clone();
    let previous_slots = state.active_interaction_slots.clone();
    let same_session = state.interaction_session_id.as_ref() == Some(&session);
    if same_session && !interaction_serial_is_valid(&state.next_interaction_serial) {
        state.active_interaction_slots.clear();
        return Err(InteractionBindError {
            code: InteractionReasonCode::InvalidAuthorityState,
        });
    }
    state.interaction_session_id = Some(session);
    if !same_session {
        state.interaction_generation = 0;
        state.next_interaction_serial = "1".to_string();
    }
    if !bind_all_current_slots(state) {
        state.interaction_session_id = previous_session;
        state.interaction_generation = previous_generation;
        state.next_interaction_serial = previous_serial;
        state.active_interaction_slots = previous_slots;
        return Err(InteractionBindError {
            code: InteractionReasonCode::InvalidAuthorityState,
        });
    }
    debug_assert_interaction_consistency(state);
    Ok(())
}

/// Preserve an existing binding exactly (used by preference-only actions).
pub(crate) fn preserve_interaction_slots(
    state: &mut GameState,
    previous: Vec<ActiveInteractionSlot>,
) {
    state.active_interaction_slots = previous;
    debug_assert_interaction_consistency(state);
}

/// Reconcile the current slots only after trusted code has explicitly bound a
/// session. Legacy and pre-contract states remain safely unbound.
pub(crate) fn ensure_interaction_authority(state: &mut GameState) {
    if state
        .interaction_session_id
        .as_ref()
        .is_none_or(|session| !interaction_session_is_valid(session))
    {
        state.active_interaction_slots.clear();
        return;
    }
    if !interaction_serial_is_valid(&state.next_interaction_serial) {
        state.active_interaction_slots.clear();
        return;
    }
    let expected = semantic_slots(&state.waiting_for);
    let matches = expected.len() == state.active_interaction_slots.len()
        && expected.iter().all(|(owner, kind)| {
            state
                .active_interaction_slots
                .iter()
                .any(|slot| slot.semantic_owner == owner.0 && slot.slot_kind == *kind)
        });
    if !matches {
        let bound = bind_all_current_slots(state);
        debug_assert!(bound);
        debug_assert_interaction_consistency(state);
    }
}

pub(crate) fn semantic_owner_for_actor(state: &GameState, actor: PlayerId) -> Option<PlayerId> {
    let acting = state.waiting_for.acting_players();
    acting
        .iter()
        .copied()
        .find(|owner| *owner == actor)
        .or_else(|| {
            acting
                .into_iter()
                .find(|owner| turn_control::authorized_submitter_for_player(state, *owner) == actor)
        })
}

pub(crate) fn action_preserves_interaction(action: &GameAction) -> bool {
    matches!(
        action,
        GameAction::SetPhaseStops { .. }
            | GameAction::SetPriorityPassingMode { .. }
            | GameAction::SetPriorityYield { .. }
            | GameAction::SetMayTriggerAutoChoice { .. }
            | GameAction::SetTriggerOrderTemplate { .. }
            | GameAction::CancelAutoPass
            | GameAction::GrantDebugPermission { .. }
            | GameAction::RevokeDebugPermission { .. }
    )
}

/// Reconcile exactly once after one accepted outward action. Single decisions
/// always rotate, including A→A and A→B→A. Simultaneous pregame decisions keep
/// every non-submitting owner's slot and rotate/remove only the submitted one.
pub(crate) fn rebind_interaction_slots_after_action(
    state: &mut GameState,
    previous_waiting: &WaitingFor,
    previous_slots: Vec<ActiveInteractionSlot>,
    submitted_owner: Option<PlayerId>,
) -> Result<(), InteractionBindError> {
    let Some(session) = state.interaction_session_id.as_ref() else {
        state.active_interaction_slots.clear();
        return Ok(());
    };
    if !interaction_session_is_valid(session)
        || !interaction_serial_is_valid(&state.next_interaction_serial)
    {
        return Err(InteractionBindError {
            code: InteractionReasonCode::InvalidAuthorityState,
        });
    }
    let prior = classify_waiting_for(previous_waiting);
    let next = semantic_slots(&state.waiting_for);
    let preserve_other_simultaneous = prior.simultaneous.is_some();
    let mut rebound = Vec::with_capacity(next.len());
    let mut needs_id = Vec::new();
    for (owner, slot_kind) in next {
        let preserved = if preserve_other_simultaneous && submitted_owner != Some(owner) {
            previous_slots
                .iter()
                .find(|slot| slot.semantic_owner == owner.0 && slot.slot_kind == slot_kind)
        } else {
            None
        };
        if let Some(slot) = preserved {
            rebound.push(slot.clone());
        } else {
            needs_id.push((rebound.len(), owner, slot_kind));
            rebound.push(ActiveInteractionSlot {
                semantic_owner: owner.0,
                slot_kind,
                interaction_id: InteractionId(String::new()),
            });
        }
    }
    let Some((ids, generation, serial)) = allocate_interaction_ids(state, needs_id.len()) else {
        return Err(InteractionBindError {
            code: InteractionReasonCode::InvalidAuthorityState,
        });
    };
    for ((index, _, _), interaction_id) in needs_id.into_iter().zip(ids) {
        rebound[index].interaction_id = interaction_id;
    }
    state.interaction_generation = generation;
    state.next_interaction_serial = serial;
    state.active_interaction_slots = rebound;
    debug_assert_interaction_consistency(state);
    Ok(())
}

pub(crate) fn debug_assert_interaction_consistency(state: &GameState) {
    #[cfg(debug_assertions)]
    {
        if state
            .interaction_session_id
            .as_ref()
            .is_none_or(|session| !interaction_session_is_valid(session))
        {
            debug_assert!(state.active_interaction_slots.is_empty());
            return;
        }
        if !interaction_serial_is_valid(&state.next_interaction_serial) {
            return;
        }
        let expected = semantic_slots(&state.waiting_for);
        debug_assert_eq!(expected.len(), state.active_interaction_slots.len());
        let mut ids = HashSet::new();
        for (owner, kind) in expected {
            let matching: Vec<_> = state
                .active_interaction_slots
                .iter()
                .filter(|slot| slot.semantic_owner == owner.0 && slot.slot_kind == kind)
                .collect();
            debug_assert_eq!(matching.len(), 1);
            if let Some(slot) = matching.first() {
                debug_assert!(ids.insert(slot.interaction_id.clone()));
            }
        }
    }
}

#[derive(Debug, Clone)]
enum SelectionAction {
    SelectCards,
    PilePartition,
    Crew { vehicle_id: ObjectId },
    Station { spacecraft_id: ObjectId },
    Saddle { mount_id: ObjectId },
    Harmonize,
    RingBearer,
    KeepWithinPower,
    KeepExact,
}

#[derive(Debug, Clone)]
struct SelectionProjection {
    object_ids: Vec<ObjectId>,
    constraint: SelectionConstraint,
    confirm: ConfirmSemantics,
    intent: InteractionIntentCode,
    action: SelectionAction,
    source_id: Option<ObjectId>,
}

#[derive(Debug, Clone)]
struct CounterAssignmentCandidate {
    object_id: ObjectId,
    counter_type: CounterType,
    available: u32,
}

#[derive(Debug, Clone)]
struct CounterDistributionProjection {
    candidates: Vec<CounterAssignmentCandidate>,
    total: u32,
}

#[derive(Debug, Clone)]
struct TriggerOrderProjection {
    count: usize,
}

#[derive(Debug, Clone, Copy)]
struct CoinFlipProjection {
    candidate_count: usize,
    keep_count: usize,
}

#[derive(Debug, Clone, Copy)]
struct NumberProjection {
    min: u32,
    max: u32,
    action: NumberResponseAction,
}

#[derive(Debug, Clone)]
enum AssignmentCandidateKind {
    Object(ObjectId),
    Target(TargetRef),
    CounterMove {
        destination_id: ObjectId,
        counter_type: CounterType,
    },
    CounterRemove {
        counter_type: CounterType,
    },
}

#[derive(Debug, Clone)]
struct AssignmentCandidate {
    kind: AssignmentCandidateKind,
    available: u32,
}

#[derive(Debug, Clone, Copy)]
enum AmountAssignmentAction {
    BlockerDamage,
    DistributeAmong,
    MoveCounters,
    RemoveCounters,
}

#[derive(Debug, Clone)]
struct AmountAssignmentProjection {
    candidates: Vec<AssignmentCandidate>,
    min_total: u32,
    max_total: u32,
    exact_total: Option<u32>,
    require_all: bool,
    action: AmountAssignmentAction,
}

#[derive(Debug, Clone)]
struct DamageAssignmentProjection {
    candidates: Vec<AssignmentCandidate>,
    total: u32,
    modes: Vec<InteractionDamageAssignmentMode>,
    blocker_count: usize,
    has_trample_target: bool,
    has_controller_target: bool,
}

#[derive(Debug, Clone, Copy)]
enum TargetSequenceAction {
    ChooseTarget,
    SelectObjects,
    SelectTargets,
    Retarget,
}

#[derive(Debug, Clone)]
struct TargetSequenceProjection {
    candidates: Vec<TargetRef>,
    min: usize,
    max: usize,
    unique: bool,
    action: TargetSequenceAction,
}

#[derive(Debug, Clone)]
struct CategorySelectionCandidate {
    group: usize,
    category: CoreType,
    object_id: ObjectId,
}

#[derive(Debug, Clone)]
struct CategorySelectionProjection {
    groups: Vec<InteractionGroupConstraint>,
    candidates: Vec<CategorySelectionCandidate>,
    source_id: ObjectId,
}

#[derive(Debug, Clone, Copy)]
enum CombatRelationTarget {
    Attack(AttackTarget),
    Object(ObjectId),
}

#[derive(Debug, Clone)]
struct CombatRelationProjection {
    action: CombatRelationAction,
    sources: Vec<ObjectId>,
    targets: Vec<CombatRelationTarget>,
    legal_target_indices: Vec<Vec<usize>>,
    max: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManaGroupCandidateValue {
    Mana(ManaType),
    Phyrexian {
        choice: ShardChoice,
        color: ManaColor,
    },
}

#[derive(Debug, Clone)]
struct ManaGroupCandidate {
    group: usize,
    value: ManaGroupCandidateValue,
}

#[derive(Debug, Clone)]
struct ManaGroupProjection {
    action: ManaGroupAction,
    groups: Vec<InteractionGroupConstraint>,
    candidates: Vec<ManaGroupCandidate>,
    max_batch: u32,
    allow_cancel: bool,
    source_id: Option<ObjectId>,
}

#[derive(Debug, Clone)]
struct ModeSequenceProjection {
    indices: Vec<usize>,
    descriptions: Vec<Option<String>>,
    min: usize,
    max: usize,
    unique: bool,
    allow_cancel: bool,
    source_id: ObjectId,
}

#[derive(Debug, Clone)]
struct OutsideSelectionCandidate {
    selection: OutsideGameSelection,
    name: String,
}

#[derive(Debug, Clone)]
struct OutsideSelectionProjection {
    candidates: Vec<OutsideSelectionCandidate>,
    min: usize,
    max: usize,
    source_id: ObjectId,
}

#[derive(Debug, Clone)]
struct TextChoiceProjection {
    options: Vec<String>,
    allow_arbitrary: bool,
    source_name: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct ShortcutReplyProjection {
    min_iteration: u32,
    max_iteration: u32,
}

#[derive(Debug, Clone)]
enum LoopShortcutCandidateValue {
    Target(TargetRef),
    ConvokeObject(ObjectId),
    Mode(usize),
    May(crate::analysis::decision_template::MayChoiceOption),
    Unless(crate::analysis::decision_template::UnlessPaymentOption),
    ManaColor(ManaColor),
}

#[derive(Debug, Clone)]
struct LoopShortcutPointProjection {
    slot: crate::analysis::decision_template::DecisionSlot,
    kind: InteractionShortcutPointKind,
    min: u32,
    max: u32,
    unique: bool,
    ordered: bool,
    read_only: bool,
    candidate_indices: Vec<usize>,
}

#[derive(Debug, Clone)]
struct LoopShortcutProjection {
    count: InteractionShortcutCountSpec,
    points: Vec<LoopShortcutPointProjection>,
    candidates: Vec<LoopShortcutCandidateValue>,
}

#[derive(Debug, Clone)]
struct DirectChoiceProjection {
    actions: Vec<GameAction>,
}

#[derive(Debug, Clone)]
struct SideboardCardProjection {
    name: String,
    total: u32,
    current_main: u32,
}

#[derive(Debug, Clone)]
struct SideboardProjection {
    cards: Vec<SideboardCardProjection>,
    main_total: u32,
}

fn target_sequence_projection(
    waiting_for: &WaitingFor,
) -> Result<Option<TargetSequenceProjection>, InteractionReasonCode> {
    let projection = match waiting_for {
        WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } => {
            let optional = target_slots
                .get(selection.current_slot)
                .is_some_and(|slot| slot.optional);
            TargetSequenceProjection {
                candidates: selection.current_legal_targets.clone(),
                min: usize::from(!optional),
                max: 1,
                unique: true,
                action: TargetSequenceAction::ChooseTarget,
            }
        }
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => {
            let optional = target_slots
                .get(selection.current_slot)
                .is_some_and(|slot| slot.optional);
            TargetSequenceProjection {
                candidates: selection.current_legal_targets.clone(),
                min: usize::from(!optional),
                max: 1,
                unique: true,
                action: TargetSequenceAction::ChooseTarget,
            }
        }
        WaitingFor::MultiTargetSelection {
            legal_targets,
            min_targets,
            max_targets,
            ..
        } => TargetSequenceProjection {
            candidates: legal_targets
                .iter()
                .copied()
                .map(TargetRef::Object)
                .collect(),
            min: *min_targets,
            max: *max_targets,
            unique: true,
            action: TargetSequenceAction::SelectObjects,
        },
        WaitingFor::ChooseObjectsSelection { eligible, .. } => TargetSequenceProjection {
            candidates: eligible.clone(),
            min: 0,
            max: eligible.len(),
            unique: true,
            action: TargetSequenceAction::SelectTargets,
        },
        WaitingFor::EachPlayerCopyChosenSelection {
            eligible, min, max, ..
        } => TargetSequenceProjection {
            candidates: eligible.clone(),
            min: *min as usize,
            max: *max as usize,
            unique: true,
            action: TargetSequenceAction::SelectTargets,
        },
        WaitingFor::ProliferateChoice { eligible, .. }
        | WaitingFor::TimeTravelChoice { eligible, .. } => TargetSequenceProjection {
            candidates: eligible.clone(),
            min: 0,
            max: eligible.len(),
            unique: true,
            action: TargetSequenceAction::SelectTargets,
        },
        WaitingFor::RetargetChoice {
            scope,
            current_targets,
            legal_new_targets,
            ..
        } => {
            let (candidates, count) = match scope {
                crate::types::game_state::RetargetScope::Single => (legal_new_targets.clone(), 1),
                crate::types::game_state::RetargetScope::All => {
                    (legal_new_targets.clone(), current_targets.len())
                }
                crate::types::game_state::RetargetScope::ForcedTo(target) => {
                    (vec![target.clone()], 1)
                }
            };
            TargetSequenceProjection {
                candidates,
                min: count,
                max: count,
                unique: false,
                action: TargetSequenceAction::Retarget,
            }
        }
        _ => return Ok(None),
    };
    if projection.candidates.len() > MAX_INTERACTION_LIST_LEN
        || projection.max > MAX_INTERACTION_LIST_LEN
    {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    Ok(Some(projection))
}

fn category_selection_projection(
    waiting_for: &WaitingFor,
) -> Result<Option<CategorySelectionProjection>, InteractionReasonCode> {
    let WaitingFor::CategoryChoice {
        categories,
        eligible_per_category,
        source_id,
        ..
    } = waiting_for
    else {
        return Ok(None);
    };
    if categories.len() != eligible_per_category.len()
        || categories.len() > MAX_INTERACTION_LIST_LEN
    {
        return Err(InteractionReasonCode::InvalidAuthorityState);
    }
    let candidate_count = eligible_per_category
        .iter()
        .try_fold(0usize, |count, candidates| {
            count.checked_add(candidates.len())
        })
        .ok_or(InteractionReasonCode::PayloadTooLarge)?;
    if candidate_count > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    let groups = eligible_per_category
        .iter()
        .enumerate()
        .map(|(group, eligible)| InteractionGroupConstraint {
            group: group as u32,
            min: u32::from(!eligible.is_empty()),
            max: u32::from(!eligible.is_empty()),
        })
        .collect();
    let candidates = categories
        .iter()
        .copied()
        .zip(eligible_per_category)
        .enumerate()
        .flat_map(|(group, (category, eligible))| {
            eligible
                .iter()
                .copied()
                .map(move |object_id| CategorySelectionCandidate {
                    group,
                    category,
                    object_id,
                })
        })
        .collect();
    Ok(Some(CategorySelectionProjection {
        groups,
        candidates,
        source_id: *source_id,
    }))
}

fn combat_relation_projection(
    waiting_for: &WaitingFor,
    expected_action: CombatRelationAction,
) -> Result<Option<CombatRelationProjection>, InteractionReasonCode> {
    let projection = match waiting_for {
        WaitingFor::DeclareAttackers {
            valid_attacker_ids,
            valid_attack_targets,
            valid_attack_targets_by_attacker,
            ..
        } if expected_action == CombatRelationAction::Attackers => {
            if valid_attacker_ids.len() > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            let legal_targets = valid_attacker_ids
                .iter()
                .map(|attacker_id| match valid_attack_targets_by_attacker {
                    Some(by_attacker) => by_attacker
                        .get(attacker_id)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    None => valid_attack_targets.as_slice(),
                })
                .collect::<Vec<_>>();
            let edge_count = legal_targets
                .iter()
                .try_fold(0usize, |count, targets| count.checked_add(targets.len()));
            if edge_count.is_none_or(|count| count > MAX_INTERACTION_LIST_LEN) {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            let mut targets = Vec::new();
            let legal_target_indices = legal_targets
                .iter()
                .map(|legal| {
                    legal
                        .iter()
                        .map(|target| {
                            if let Some(index) = targets.iter().position(|candidate| {
                                matches!(candidate, CombatRelationTarget::Attack(candidate) if candidate == target)
                            }) {
                                index
                            } else {
                                targets.push(CombatRelationTarget::Attack(*target));
                                targets.len() - 1
                            }
                        })
                        .collect()
                })
                .collect();
            if targets.len() > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            CombatRelationProjection {
                action: CombatRelationAction::Attackers,
                sources: valid_attacker_ids.clone(),
                targets,
                legal_target_indices,
                max: valid_attacker_ids.len(),
            }
        }
        WaitingFor::DeclareBlockers {
            valid_blocker_ids,
            valid_block_targets,
            ..
        } if expected_action == CombatRelationAction::Blockers => {
            if valid_blocker_ids.len() > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            let legal_targets = valid_blocker_ids
                .iter()
                .map(|blocker_id| {
                    valid_block_targets
                        .get(blocker_id)
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>();
            let edge_count = legal_targets
                .iter()
                .try_fold(0usize, |count, targets| count.checked_add(targets.len()))
                .ok_or(InteractionReasonCode::PayloadTooLarge)?;
            if edge_count > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            let mut targets = Vec::new();
            let legal_target_indices = legal_targets
                .iter()
                .map(|legal| {
                    legal
                        .iter()
                        .map(|target| {
                            if let Some(index) = targets.iter().position(|candidate| {
                                matches!(candidate, CombatRelationTarget::Object(candidate) if candidate == target)
                            }) {
                                index
                            } else {
                                targets.push(CombatRelationTarget::Object(*target));
                                targets.len() - 1
                            }
                        })
                        .collect()
                })
                .collect();
            CombatRelationProjection {
                action: CombatRelationAction::Blockers,
                sources: valid_blocker_ids.clone(),
                targets,
                legal_target_indices,
                max: edge_count,
            }
        }
        WaitingFor::DeclareAttackers { .. } | WaitingFor::DeclareBlockers { .. } => {
            return Err(InteractionReasonCode::InvalidAuthorityState);
        }
        _ => return Ok(None),
    };
    let total_choices = projection
        .sources
        .len()
        .checked_add(projection.targets.len())
        .ok_or(InteractionReasonCode::PayloadTooLarge)?;
    if total_choices > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    Ok(Some(projection))
}

fn mana_route_projection(
    action: ManaGroupAction,
    routes: &[Vec<ManaType>],
    source_id: Option<ObjectId>,
) -> Result<ManaGroupProjection, InteractionReasonCode> {
    let Some(width) = routes.first().map(Vec::len) else {
        return Err(InteractionReasonCode::InvalidAuthorityState);
    };
    let element_count = routes.iter().try_fold(0usize, |count, route| {
        if route.len() != width {
            None
        } else {
            count.checked_add(route.len())
        }
    });
    if width > MAX_INTERACTION_LIST_LEN
        || element_count.is_none_or(|count| count > MAX_INTERACTION_LIST_LEN)
    {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    let mut candidates = Vec::new();
    for group in 0..width {
        let mut values = Vec::new();
        for route in routes {
            if !values.contains(&route[group]) {
                values.push(route[group]);
            }
        }
        candidates.extend(values.into_iter().map(|value| ManaGroupCandidate {
            group,
            value: ManaGroupCandidateValue::Mana(value),
        }));
    }
    Ok(ManaGroupProjection {
        action,
        groups: (0..width)
            .map(|group| InteractionGroupConstraint {
                group: group as u32,
                min: 1,
                max: 1,
            })
            .collect(),
        candidates,
        max_batch: 1,
        allow_cancel: false,
        source_id,
    })
}

fn mana_group_projection(
    waiting_for: &WaitingFor,
    expected_action: ManaGroupAction,
) -> Result<Option<ManaGroupProjection>, InteractionReasonCode> {
    let projection = match waiting_for {
        WaitingFor::PayManaAbilityMana {
            options,
            pending_mana_ability,
            ..
        } if expected_action == ManaGroupAction::PayManaAbility => mana_route_projection(
            expected_action,
            options,
            Some(pending_mana_ability.source_id),
        )?,
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options },
            context,
            ..
        } if expected_action == ManaGroupAction::ChooseSingleColor => {
            if options.len() > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            let (max_batch, source_id) = match context {
                ManaChoiceContext::ManaAbility(pending) => (
                    pending
                        .batch_siblings
                        .len()
                        .saturating_add(1)
                        .min(u32::MAX as usize) as u32,
                    Some(pending.source_id),
                ),
                ManaChoiceContext::ResolvingEffect(_) => (1, None),
            };
            ManaGroupProjection {
                action: expected_action,
                groups: vec![InteractionGroupConstraint {
                    group: 0,
                    min: 1,
                    max: 1,
                }],
                candidates: options
                    .iter()
                    .copied()
                    .map(|value| ManaGroupCandidate {
                        group: 0,
                        value: ManaGroupCandidateValue::Mana(value),
                    })
                    .collect(),
                max_batch,
                allow_cancel: false,
                source_id,
            }
        }
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::Combination { options },
            ..
        } if expected_action == ManaGroupAction::ChooseCombination => {
            mana_route_projection(expected_action, options, None)?
        }
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::AnyCombination { count, options },
            ..
        } if expected_action == ManaGroupAction::ChooseAnyCombination => {
            let candidate_count = count
                .checked_mul(options.len())
                .ok_or(InteractionReasonCode::PayloadTooLarge)?;
            if *count > MAX_INTERACTION_LIST_LEN || candidate_count > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            ManaGroupProjection {
                action: expected_action,
                groups: (0..*count)
                    .map(|group| InteractionGroupConstraint {
                        group: group as u32,
                        min: 1,
                        max: 1,
                    })
                    .collect(),
                candidates: (0..*count)
                    .flat_map(|group| {
                        options
                            .iter()
                            .copied()
                            .map(move |value| ManaGroupCandidate {
                                group,
                                value: ManaGroupCandidateValue::Mana(value),
                            })
                    })
                    .collect(),
                max_batch: 1,
                allow_cancel: false,
                source_id: None,
            }
        }
        WaitingFor::PhyrexianPayment {
            spell_object,
            shards,
            ..
        } if expected_action == ManaGroupAction::Phyrexian => {
            if shards.len() > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            let candidates = shards
                .iter()
                .enumerate()
                .flat_map(|(group, shard)| {
                    let values = match shard.options {
                        ShardOptions::ManaOrLife => {
                            &[ShardChoice::PayMana, ShardChoice::PayLife][..]
                        }
                        ShardOptions::ManaOnly => &[ShardChoice::PayMana][..],
                        ShardOptions::LifeOnly => &[ShardChoice::PayLife][..],
                    };
                    values
                        .iter()
                        .copied()
                        .map(move |choice| ManaGroupCandidate {
                            group,
                            value: ManaGroupCandidateValue::Phyrexian {
                                choice,
                                color: shard.color,
                            },
                        })
                })
                .collect::<Vec<_>>();
            if candidates.len() > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            ManaGroupProjection {
                action: expected_action,
                groups: (0..shards.len())
                    .map(|group| InteractionGroupConstraint {
                        group: group as u32,
                        min: 1,
                        max: 1,
                    })
                    .collect(),
                candidates,
                max_batch: 1,
                allow_cancel: true,
                source_id: Some(*spell_object),
            }
        }
        WaitingFor::PayManaAbilityMana { .. }
        | WaitingFor::ChooseManaColor { .. }
        | WaitingFor::PhyrexianPayment { .. } => {
            return Err(InteractionReasonCode::InvalidAuthorityState);
        }
        _ => return Ok(None),
    };
    if projection.candidates.len() > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    Ok(Some(projection))
}

fn mode_sequence_projection(
    waiting_for: &WaitingFor,
) -> Result<Option<ModeSequenceProjection>, InteractionReasonCode> {
    let (modal, unavailable_modes, allow_cancel, source_id) = match waiting_for {
        WaitingFor::ModeChoice {
            modal,
            unavailable_modes,
            pending_cast,
            ..
        } => (modal, unavailable_modes, true, pending_cast.object_id),
        WaitingFor::AbilityModeChoice {
            modal,
            unavailable_modes,
            is_activated,
            source_id,
            ..
        } => (modal, unavailable_modes, *is_activated, *source_id),
        _ => return Ok(None),
    };
    if modal.mode_count > MAX_INTERACTION_LIST_LEN
        || modal
            .mode_descriptions
            .iter()
            .any(|description| description.len() > MAX_INTERACTION_STRING_LEN)
        || (!modal.mode_pawprints.is_empty() && modal.mode_pawprints.len() < modal.mode_count)
    {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    let indices = (0..modal.mode_count)
        .filter(|index| !unavailable_modes.contains(index))
        .collect::<Vec<_>>();
    let max = if modal.mode_pawprints.is_empty() {
        if modal.allow_repeat_modes {
            modal.max_choices
        } else {
            modal.max_choices.min(indices.len())
        }
    } else if modal.allow_repeat_modes {
        let minimum_weight = indices
            .iter()
            .map(|index| modal.mode_pawprints[*index] as usize)
            .min()
            .ok_or(InteractionReasonCode::InvalidAuthorityState)?;
        if minimum_weight == 0 {
            return Err(InteractionReasonCode::PayloadTooLarge);
        }
        modal.max_choices / minimum_weight
    } else {
        indices.len()
    };
    if max > MAX_INTERACTION_LIST_LEN || modal.min_choices > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    let descriptions = indices
        .iter()
        .map(|index| modal.mode_descriptions.get(*index).cloned())
        .collect();
    Ok(Some(ModeSequenceProjection {
        indices,
        descriptions,
        min: modal.min_choices,
        max,
        unique: !modal.allow_repeat_modes,
        allow_cancel,
        source_id,
    }))
}

fn outside_selection_projection(
    waiting_for: &WaitingFor,
) -> Result<Option<OutsideSelectionProjection>, InteractionReasonCode> {
    let WaitingFor::OutsideGameChoice {
        choices,
        count,
        up_to,
        source_id,
        ..
    } = waiting_for
    else {
        return Ok(None);
    };
    let candidate_count = choices.iter().try_fold(0usize, |total, choice| {
        if choice.name.len() > MAX_INTERACTION_STRING_LEN {
            None
        } else {
            total.checked_add(match &choice.source {
                OutsideGameChoiceSource::Sideboard { .. } => choice.count as usize,
                OutsideGameChoiceSource::FaceUpExile { .. } => 1,
            })
        }
    });
    if *count > MAX_INTERACTION_LIST_LEN
        || candidate_count.is_none_or(|count| count > MAX_INTERACTION_LIST_LEN)
    {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    let candidates = choices
        .iter()
        .flat_map(|choice| {
            let selection = match &choice.source {
                OutsideGameChoiceSource::Sideboard {
                    sideboard_index, ..
                } => OutsideGameSelection::Sideboard {
                    sideboard_index: *sideboard_index,
                },
                OutsideGameChoiceSource::FaceUpExile { object_id } => {
                    OutsideGameSelection::FaceUpExile {
                        object_id: *object_id,
                    }
                }
            };
            let copies = match &choice.source {
                OutsideGameChoiceSource::Sideboard { .. } => choice.count as usize,
                OutsideGameChoiceSource::FaceUpExile { .. } => 1,
            };
            (0..copies).map(move |_| OutsideSelectionCandidate {
                selection: selection.clone(),
                name: choice.name.clone(),
            })
        })
        .collect();
    Ok(Some(OutsideSelectionProjection {
        candidates,
        min: if *up_to { 0 } else { *count },
        max: *count,
        source_id: *source_id,
    }))
}

fn text_choice_projection(
    waiting_for: &WaitingFor,
) -> Result<Option<TextChoiceProjection>, InteractionReasonCode> {
    let WaitingFor::NamedChoice {
        choice_type,
        options,
        source,
        ..
    } = waiting_for
    else {
        return Ok(None);
    };
    if options.len() > MAX_INTERACTION_LIST_LEN
        || options
            .iter()
            .any(|option| option.len() > MAX_INTERACTION_STRING_LEN)
        || source
            .as_ref()
            .is_some_and(|source| source.prompt.display_name.len() > MAX_INTERACTION_STRING_LEN)
    {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    Ok(Some(TextChoiceProjection {
        options: options.clone(),
        allow_arbitrary: matches!(choice_type, ChoiceType::CardName),
        source_name: source
            .as_ref()
            .map(|source| source.prompt.display_name.clone()),
    }))
}

fn shortcut_reply_projection(waiting_for: &WaitingFor) -> Option<ShortcutReplyProjection> {
    let WaitingFor::RespondToShortcut { proposal, .. } = waiting_for else {
        return None;
    };
    let max_iteration = match proposal.count {
        crate::analysis::decision_template::IterationCount::Fixed(iterations) => {
            iterations.saturating_sub(1)
        }
        crate::analysis::decision_template::IterationCount::UntilLethal => u32::MAX,
    };
    Some(ShortcutReplyProjection {
        min_iteration: 0,
        max_iteration,
    })
}

fn mana_payment_direct_actions(
    state: &GameState,
    player: PlayerId,
    convoke_mode: Option<ConvokeMode>,
) -> Result<Vec<GameAction>, InteractionReasonCode> {
    let activation_upper_bound = state
        .battlefield
        .iter()
        .try_fold(0usize, |count, object_id| {
            let object = state.objects.get(object_id)?;
            count.checked_add(object.abilities.len().saturating_add(1))
        });
    if activation_upper_bound.is_none_or(|count| count > MAX_INTERACTION_LIST_LEN) {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    let mut actions = super::mana_sources::activatable_mana_actions_for_player(state, player);
    let tapped_for_mana = state
        .lands_tapped_for_mana
        .get(&player)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let pool = state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .ok_or(InteractionReasonCode::InvalidAuthorityState)?;
    let pinned = state
        .pending_cast
        .as_ref()
        .map(|pending| pending.pinned_pool_units.as_slice())
        .unwrap_or_default();
    let convoke_upper_bound = match convoke_mode {
        None => 0,
        Some(ConvokeMode::Delve) => state
            .objects
            .values()
            .filter(|object| object.is_delve_eligible(player))
            .count(),
        Some(mode) => state
            .battlefield
            .iter()
            .filter_map(|object_id| state.objects.get(object_id))
            .filter(|object| !crate::game::restrictions::object_cant_tap(state, object.id))
            .map(|object| match mode {
                ConvokeMode::Convoke if object.is_convoke_eligible(player) => {
                    1usize.saturating_add(object.color.len())
                }
                ConvokeMode::Waterbend if object.is_waterbend_eligible(player) => 1,
                ConvokeMode::Improvise if object.is_improvise_eligible(player) => 1,
                ConvokeMode::Convoke | ConvokeMode::Waterbend | ConvokeMode::Improvise => 0,
                ConvokeMode::Delve => unreachable!("delve counted from all zone objects"),
            })
            .try_fold(0usize, |count, choices| count.checked_add(choices))
            .ok_or(InteractionReasonCode::PayloadTooLarge)?,
    };
    let total_upper_bound = actions
        .len()
        .checked_add(tapped_for_mana.len())
        .and_then(|count| count.checked_add(pool.mana_pool.mana.len()))
        .and_then(|count| count.checked_add(convoke_upper_bound))
        .and_then(|count| count.checked_add(2))
        .ok_or(InteractionReasonCode::PayloadTooLarge)?;
    if total_upper_bound > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    actions.push(GameAction::PassPriority);
    actions.push(GameAction::CancelCast);
    let mut undo_seen = HashSet::with_capacity(tapped_for_mana.len());
    actions.extend(
        tapped_for_mana
            .iter()
            .copied()
            .filter(|object_id| undo_seen.insert(*object_id))
            .map(|object_id| GameAction::UntapLandForMana { object_id }),
    );
    actions.extend(
        pool.mana_pool
            .mana
            .iter()
            .filter(|unit| unit.pip_id.0 != 0)
            .map(|unit| {
                if pinned.contains(&unit.pip_id) {
                    GameAction::UnspendPoolMana {
                        pip_id: unit.pip_id,
                    }
                } else {
                    GameAction::SpendPoolMana {
                        pip_id: unit.pip_id,
                    }
                }
            }),
    );
    match convoke_mode {
        None => {}
        Some(ConvokeMode::Delve) => {
            actions.extend(state.objects.values().filter_map(|object| {
                object
                    .is_delve_eligible(player)
                    .then_some(GameAction::TapForConvoke {
                        object_id: object.id,
                        mana_type: ManaType::Colorless,
                    })
            }));
        }
        Some(mode) => {
            let cost_shards = state
                .pending_cast
                .as_ref()
                .and_then(|pending| match &pending.cost {
                    ManaCost::Cost { shards, .. } => Some(shards.as_slice()),
                    ManaCost::NoCost
                    | ManaCost::SelfManaCost
                    | ManaCost::SelfManaValue
                    | ManaCost::SelfManaCostReduced { .. } => None,
                });
            for object_id in &state.battlefield {
                let Some(object) = state.objects.get(object_id) else {
                    continue;
                };
                if crate::game::restrictions::object_cant_tap(state, *object_id) {
                    continue;
                }
                match mode {
                    ConvokeMode::Convoke if object.is_convoke_eligible(player) => {
                        actions.push(GameAction::TapForConvoke {
                            object_id: *object_id,
                            mana_type: ManaType::Colorless,
                        });
                        actions.extend(object.color.iter().filter_map(|color| {
                            if cost_shards.is_some_and(|shards| {
                                !shards.iter().any(|shard| shard.contributes_to(*color))
                            }) {
                                None
                            } else {
                                Some(GameAction::TapForConvoke {
                                    object_id: *object_id,
                                    mana_type: super::mana_sources::mana_color_to_type(color),
                                })
                            }
                        }));
                    }
                    ConvokeMode::Waterbend if object.is_waterbend_eligible(player) => {
                        actions.push(GameAction::TapForConvoke {
                            object_id: *object_id,
                            mana_type: ManaType::Colorless,
                        });
                    }
                    ConvokeMode::Improvise if object.is_improvise_eligible(player) => {
                        actions.push(GameAction::TapForConvoke {
                            object_id: *object_id,
                            mana_type: ManaType::Colorless,
                        });
                    }
                    ConvokeMode::Convoke | ConvokeMode::Waterbend | ConvokeMode::Improvise => {}
                    ConvokeMode::Delve => unreachable!("delve handled separately"),
                }
            }
        }
    }
    Ok(actions)
}

fn direct_choice_projection(
    waiting_for: &WaitingFor,
    state: &GameState,
    semantic_owner: PlayerId,
) -> Result<Option<DirectChoiceProjection>, InteractionReasonCode> {
    let actions = match waiting_for {
        WaitingFor::ManaPayment {
            player,
            convoke_mode,
        } => {
            if *player != semantic_owner {
                return Err(InteractionReasonCode::InvalidAuthorityState);
            }
            mana_payment_direct_actions(state, *player, *convoke_mode)?
        }
        WaitingFor::PrecastCopyShortcutOffer {
            epoch, route_count, ..
        } => {
            if *route_count != 1 {
                return Err(InteractionReasonCode::InvalidAuthorityState);
            }
            vec![
                GameAction::PrecastCopyShortcut {
                    epoch: *epoch,
                    response: PrecastCopyShortcutResponse::Propose { route_id: *epoch },
                },
                GameAction::PrecastCopyShortcut {
                    epoch: *epoch,
                    response: PrecastCopyShortcutResponse::Decline,
                },
            ]
        }
        WaitingFor::RespondToPrecastCopyShortcut {
            epoch,
            breakpoint_ids,
            ..
        } => {
            if breakpoint_ids.len() > MAX_INTERACTION_LIST_LEN
                || breakpoint_ids.iter().collect::<HashSet<_>>().len() != breakpoint_ids.len()
            {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            std::iter::once(GameAction::PrecastCopyShortcut {
                epoch: *epoch,
                response: PrecastCopyShortcutResponse::Accept,
            })
            .chain(
                breakpoint_ids
                    .iter()
                    .map(|breakpoint_id| GameAction::PrecastCopyShortcut {
                        epoch: *epoch,
                        response: PrecastCopyShortcutResponse::Shorten {
                            breakpoint_id: *breakpoint_id,
                        },
                    }),
            )
            .collect()
        }
        WaitingFor::CommanderZoneChoice { player, .. } => {
            if *player != semantic_owner {
                return Err(InteractionReasonCode::InvalidAuthorityState);
            }
            vec![
                GameAction::DecideOptionalEffect { accept: true },
                GameAction::DecideOptionalEffect { accept: false },
            ]
        }
        WaitingFor::UntapChoice {
            player, candidates, ..
        } => {
            if *player != semantic_owner {
                return Err(InteractionReasonCode::InvalidAuthorityState);
            }
            if candidates
                .len()
                .checked_mul(2)
                .is_none_or(|count| count > MAX_INTERACTION_LIST_LEN)
            {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            candidates
                .iter()
                .flat_map(|object_id| {
                    [true, false].map(|untap| GameAction::ChooseUntap {
                        object_id: *object_id,
                        untap,
                    })
                })
                .collect()
        }
        _ => return Ok(None),
    };
    Ok(Some(DirectChoiceProjection { actions }))
}

fn sideboard_projection(
    waiting_for: &WaitingFor,
    state: &GameState,
    semantic_owner: PlayerId,
) -> Result<Option<SideboardProjection>, InteractionReasonCode> {
    let WaitingFor::BetweenGamesSideboard { player, .. } = waiting_for else {
        return Ok(None);
    };
    if *player != semantic_owner {
        return Err(InteractionReasonCode::InvalidAuthorityState);
    }
    let pool = state
        .deck_pools
        .iter()
        .find(|pool| pool.player == semantic_owner)
        .ok_or(InteractionReasonCode::InvalidAuthorityState)?;
    let mut totals = BTreeMap::<String, u32>::new();
    for entry in pool
        .registered_main
        .iter()
        .chain(pool.registered_sideboard.iter())
    {
        if entry.card.name.len() > MAX_INTERACTION_STRING_LEN {
            return Err(InteractionReasonCode::PayloadTooLarge);
        }
        let total = totals.entry(entry.card.name.clone()).or_default();
        *total = total
            .checked_add(entry.count)
            .ok_or(InteractionReasonCode::PayloadTooLarge)?;
    }
    if totals.len() > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    let mut current_main = BTreeMap::<&str, u32>::new();
    for entry in pool.current_main.iter() {
        let count = current_main.entry(entry.card.name.as_str()).or_default();
        *count = count
            .checked_add(entry.count)
            .ok_or(InteractionReasonCode::PayloadTooLarge)?;
    }
    let main_total = pool
        .registered_main
        .iter()
        .try_fold(0u32, |total, entry| total.checked_add(entry.count));
    let Some(main_total) = main_total else {
        return Err(InteractionReasonCode::PayloadTooLarge);
    };
    let cards = totals
        .into_iter()
        .map(|(name, total)| SideboardCardProjection {
            current_main: current_main.get(name.as_str()).copied().unwrap_or(0),
            name,
            total,
        })
        .collect();
    Ok(Some(SideboardProjection { cards, main_total }))
}

fn attack_target_ref(target: &AttackTarget) -> TargetRef {
    match target {
        AttackTarget::Player(player) => TargetRef::Player(*player),
        AttackTarget::Planeswalker(object_id) | AttackTarget::Battle(object_id) => {
            TargetRef::Object(*object_id)
        }
    }
}

fn amount_assignment_projection(
    waiting_for: &WaitingFor,
) -> Result<Option<AmountAssignmentProjection>, InteractionReasonCode> {
    let projection = match waiting_for {
        WaitingFor::AssignBlockerDamage {
            total_damage,
            attackers,
            ..
        } => AmountAssignmentProjection {
            candidates: attackers
                .iter()
                .copied()
                .map(|object_id| AssignmentCandidate {
                    kind: AssignmentCandidateKind::Object(object_id),
                    available: *total_damage,
                })
                .collect(),
            min_total: *total_damage,
            max_total: *total_damage,
            exact_total: Some(*total_damage),
            require_all: false,
            action: AmountAssignmentAction::BlockerDamage,
        },
        WaitingFor::DistributeAmong { total, targets, .. } => AmountAssignmentProjection {
            candidates: targets
                .iter()
                .cloned()
                .map(|target| AssignmentCandidate {
                    kind: AssignmentCandidateKind::Target(target),
                    available: *total,
                })
                .collect(),
            min_total: *total,
            max_total: *total,
            exact_total: Some(*total),
            require_all: true,
            action: AmountAssignmentAction::DistributeAmong,
        },
        WaitingFor::MoveCountersDistribution {
            available,
            destinations,
            ..
        } => {
            let count = available
                .len()
                .checked_mul(destinations.len())
                .ok_or(InteractionReasonCode::PayloadTooLarge)?;
            if count > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            let max_total = available
                .iter()
                .try_fold(0u32, |total, (_, count)| total.checked_add(*count))
                .ok_or(InteractionReasonCode::PayloadTooLarge)?;
            AmountAssignmentProjection {
                candidates: available
                    .iter()
                    .flat_map(|(counter_type, count)| {
                        destinations
                            .iter()
                            .map(move |destination_id| AssignmentCandidate {
                                kind: AssignmentCandidateKind::CounterMove {
                                    destination_id: *destination_id,
                                    counter_type: counter_type.clone(),
                                },
                                available: *count,
                            })
                    })
                    .collect(),
                min_total: 0,
                max_total,
                exact_total: None,
                require_all: false,
                action: AmountAssignmentAction::MoveCounters,
            }
        }
        WaitingFor::RemoveCountersChoice { available, .. } => {
            let max_total = available
                .iter()
                .try_fold(0u32, |total, (_, count)| total.checked_add(*count))
                .ok_or(InteractionReasonCode::PayloadTooLarge)?;
            AmountAssignmentProjection {
                candidates: available
                    .iter()
                    .map(|(counter_type, count)| AssignmentCandidate {
                        kind: AssignmentCandidateKind::CounterRemove {
                            counter_type: counter_type.clone(),
                        },
                        available: *count,
                    })
                    .collect(),
                min_total: 0,
                max_total,
                exact_total: None,
                require_all: false,
                action: AmountAssignmentAction::RemoveCounters,
            }
        }
        _ => return Ok(None),
    };
    if projection.candidates.len() > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    Ok(Some(projection))
}

fn damage_assignment_projection(
    waiting_for: &WaitingFor,
) -> Result<Option<DamageAssignmentProjection>, InteractionReasonCode> {
    let WaitingFor::AssignCombatDamage {
        total_damage,
        blockers,
        assignment_modes,
        trample,
        attack_target,
        pw_controller,
        ..
    } = waiting_for
    else {
        return Ok(None);
    };
    let has_trample_target = trample.is_some();
    let has_controller_target = pw_controller.is_some();
    let count =
        blockers.len() + usize::from(has_trample_target) + usize::from(has_controller_target);
    if count > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    let mut candidates: Vec<_> = blockers
        .iter()
        .map(|slot| AssignmentCandidate {
            kind: AssignmentCandidateKind::Object(slot.blocker_id),
            available: *total_damage,
        })
        .collect();
    if has_trample_target {
        candidates.push(AssignmentCandidate {
            kind: AssignmentCandidateKind::Target(attack_target_ref(attack_target)),
            available: *total_damage,
        });
    }
    if let Some(controller) = pw_controller {
        candidates.push(AssignmentCandidate {
            kind: AssignmentCandidateKind::Target(TargetRef::Player(*controller)),
            available: *total_damage,
        });
    }
    let mut modes: Vec<_> = assignment_modes
        .iter()
        .map(|mode| match mode {
            CombatDamageAssignmentMode::Normal => InteractionDamageAssignmentMode::Normal,
            CombatDamageAssignmentMode::AsThoughUnblocked => {
                InteractionDamageAssignmentMode::AsThoughUnblocked
            }
        })
        .collect();
    if !modes.contains(&InteractionDamageAssignmentMode::Normal) {
        modes.insert(0, InteractionDamageAssignmentMode::Normal);
    }
    Ok(Some(DamageAssignmentProjection {
        candidates,
        total: *total_damage,
        modes,
        blocker_count: blockers.len(),
        has_trample_target,
        has_controller_target,
    }))
}

/// CR 603.3b: the controller may choose any permutation of their simultaneous triggers.
fn trigger_order_projection(
    waiting_for: &WaitingFor,
) -> Result<Option<TriggerOrderProjection>, InteractionReasonCode> {
    let WaitingFor::OrderTriggers { triggers, .. } = waiting_for else {
        return Ok(None);
    };
    if triggers.len() > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    Ok(Some(TriggerOrderProjection {
        count: triggers.len(),
    }))
}

fn coin_flip_projection(
    waiting_for: &WaitingFor,
) -> Result<Option<CoinFlipProjection>, InteractionReasonCode> {
    let WaitingFor::CoinFlipKeepChoice {
        results,
        keep_count,
        ..
    } = waiting_for
    else {
        return Ok(None);
    };
    if results.len() > MAX_INTERACTION_LIST_LEN || *keep_count > results.len() {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    Ok(Some(CoinFlipProjection {
        candidate_count: results.len(),
        keep_count: *keep_count,
    }))
}

fn number_projection(waiting_for: &WaitingFor) -> Option<NumberProjection> {
    match waiting_for {
        WaitingFor::ChooseXValue { min, max, .. } => Some(NumberProjection {
            min: *min,
            max: *max,
            action: NumberResponseAction::ChooseX,
        }),
        WaitingFor::PayAmountChoice { min, max, .. } => Some(NumberProjection {
            min: *min,
            max: *max,
            action: NumberResponseAction::PayAmount,
        }),
        WaitingFor::AssistPayment { max_generic, .. } => Some(NumberProjection {
            min: 0,
            max: *max_generic,
            action: NumberResponseAction::AssistPayment,
        }),
        _ => None,
    }
}

fn loop_shortcut_projection(
    waiting_for: &WaitingFor,
) -> Result<LoopShortcutProjection, InteractionReasonCode> {
    use crate::analysis::decision_template::DecisionPointKind;

    let WaitingFor::LoopShortcut { schema, .. } = waiting_for else {
        return Err(InteractionReasonCode::UnsupportedResponse);
    };
    if schema.points.len() > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    if schema
        .points
        .iter()
        .map(|point| &point.slot)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != schema.points.len()
    {
        return Err(InteractionReasonCode::InvalidAuthorityState);
    }
    let mut source_multiplicities = BTreeMap::new();
    for point in &schema.points {
        let multiplicity = source_multiplicities
            .entry(&point.slot.source)
            .or_insert(0usize);
        *multiplicity += 1;
        if *multiplicity > u8::MAX as usize {
            return Err(InteractionReasonCode::InvalidAuthorityState);
        }
    }
    let count = match schema.iteration_count {
        crate::analysis::decision_template::IterationCount::Fixed(suggested) => {
            InteractionShortcutCountSpec::Fixed {
                min: 1,
                max: MAX_SHORTCUT_CYCLES,
                suggested: suggested.clamp(1, MAX_SHORTCUT_CYCLES),
            }
        }
        crate::analysis::decision_template::IterationCount::UntilLethal => {
            InteractionShortcutCountSpec::UntilLethal
        }
    };
    let mut candidates = Vec::new();
    let mut points = Vec::with_capacity(schema.points.len());
    for point in &schema.points {
        let start = candidates.len();
        let (kind, min, max, unique, ordered, read_only) = match &point.kind {
            DecisionPointKind::Targets {
                legal_targets,
                min_targets,
                max_targets,
                ordered,
            } => {
                if legal_targets.len() > MAX_INTERACTION_LIST_LEN {
                    return Err(InteractionReasonCode::PayloadTooLarge);
                }
                if min_targets > max_targets
                    || *max_targets as usize > MAX_INTERACTION_LIST_LEN
                    || (*max_targets > 0 && legal_targets.is_empty())
                {
                    return Err(InteractionReasonCode::InvalidAuthorityState);
                }
                candidates.extend(
                    legal_targets
                        .iter()
                        .cloned()
                        .map(LoopShortcutCandidateValue::Target),
                );
                (
                    InteractionShortcutPointKind::Targets,
                    *min_targets,
                    *max_targets,
                    false,
                    *ordered,
                    false,
                )
            }
            DecisionPointKind::ConvokeTaps { tappable } => {
                if tappable.len() > MAX_INTERACTION_LIST_LEN {
                    return Err(InteractionReasonCode::PayloadTooLarge);
                }
                candidates.extend(
                    tappable
                        .iter()
                        .copied()
                        .map(LoopShortcutCandidateValue::ConvokeObject),
                );
                (
                    InteractionShortcutPointKind::ConvokeTaps,
                    0,
                    0,
                    true,
                    false,
                    true,
                )
            }
            DecisionPointKind::Mode {
                available_modes,
                min_modes,
                max_modes,
                allow_repeats,
            } => {
                if available_modes.len() > MAX_INTERACTION_LIST_LEN {
                    return Err(InteractionReasonCode::PayloadTooLarge);
                }
                if min_modes > max_modes
                    || *max_modes as usize > MAX_INTERACTION_LIST_LEN
                    || (*max_modes > 0 && available_modes.is_empty())
                    || available_modes
                        .iter()
                        .any(|mode| u32::try_from(*mode).is_err())
                    || available_modes.iter().collect::<HashSet<_>>().len() != available_modes.len()
                    || (!allow_repeats && *max_modes as usize > available_modes.len())
                {
                    return Err(InteractionReasonCode::InvalidAuthorityState);
                }
                candidates.extend(
                    available_modes
                        .iter()
                        .copied()
                        .map(LoopShortcutCandidateValue::Mode),
                );
                (
                    InteractionShortcutPointKind::Mode,
                    *min_modes,
                    *max_modes,
                    !*allow_repeats,
                    true,
                    false,
                )
            }
            DecisionPointKind::MayChoice => {
                candidates.extend([
                    LoopShortcutCandidateValue::May(
                        crate::analysis::decision_template::MayChoiceOption::Take,
                    ),
                    LoopShortcutCandidateValue::May(
                        crate::analysis::decision_template::MayChoiceOption::Decline,
                    ),
                ]);
                (
                    InteractionShortcutPointKind::MayChoice,
                    1,
                    1,
                    true,
                    false,
                    false,
                )
            }
            DecisionPointKind::UnlessBreak => {
                candidates.extend([
                    LoopShortcutCandidateValue::Unless(
                        crate::analysis::decision_template::UnlessPaymentOption::Pay,
                    ),
                    LoopShortcutCandidateValue::Unless(
                        crate::analysis::decision_template::UnlessPaymentOption::Decline,
                    ),
                ]);
                (
                    InteractionShortcutPointKind::UnlessBreak,
                    1,
                    1,
                    true,
                    false,
                    false,
                )
            }
            DecisionPointKind::ManaColor { color } => {
                candidates.push(LoopShortcutCandidateValue::ManaColor(*color));
                (
                    InteractionShortcutPointKind::ManaColor,
                    0,
                    0,
                    true,
                    false,
                    true,
                )
            }
        };
        if candidates.len() > MAX_INTERACTION_LIST_LEN {
            return Err(InteractionReasonCode::PayloadTooLarge);
        }
        points.push(LoopShortcutPointProjection {
            slot: point.slot.clone(),
            kind,
            min,
            max,
            unique,
            ordered,
            read_only,
            candidate_indices: (start..candidates.len()).collect(),
        });
    }
    Ok(LoopShortcutProjection {
        count,
        points,
        candidates,
    })
}

fn comparator_dto(comparator: Comparator) -> AggregateComparator {
    match comparator {
        Comparator::GT => AggregateComparator::GreaterThan,
        Comparator::LT => AggregateComparator::LessThan,
        Comparator::GE => AggregateComparator::AtLeast,
        Comparator::LE => AggregateComparator::AtMost,
        Comparator::EQ => AggregateComparator::Equal,
        Comparator::NE => AggregateComparator::NotEqual,
    }
}

fn count_constraint(min: usize, max: usize) -> SelectionConstraint {
    SelectionConstraint::Count {
        min: min.min(u32::MAX as usize) as u32,
        max: max.min(u32::MAX as usize) as u32,
    }
}

fn effect_zone_intent(effect_kind: EffectKind, destination: Option<Zone>) -> InteractionIntentCode {
    if effect_kind == EffectKind::Sacrifice && destination.is_none() {
        InteractionIntentCode::Sacrifice
    } else {
        match destination {
            Some(Zone::Hand) => InteractionIntentCode::Return,
            Some(Zone::Exile) => InteractionIntentCode::Exile,
            Some(Zone::Battlefield)
            | Some(Zone::Library)
            | Some(Zone::Graveyard)
            | Some(Zone::Stack)
            | Some(Zone::Command)
            | None => InteractionIntentCode::Choose,
        }
    }
}

fn pay_cost_intent(kind: &PayCostKind) -> InteractionIntentCode {
    match kind {
        PayCostKind::Discard | PayCostKind::Reveal | PayCostKind::Behold { .. } => {
            InteractionIntentCode::Pay
        }
        PayCostKind::Sacrifice => InteractionIntentCode::Sacrifice,
        PayCostKind::ReturnToHand => InteractionIntentCode::Return,
        PayCostKind::ExileFromZone { .. }
        | PayCostKind::ExileMaterials { .. }
        | PayCostKind::ExilePermanent { .. }
        | PayCostKind::ExileFromManaZone { .. }
        | PayCostKind::ExileAggregate { .. } => InteractionIntentCode::Exile,
        PayCostKind::UnattachFrom { .. } => InteractionIntentCode::Choose,
        PayCostKind::RemoveCounter { .. } => InteractionIntentCode::Pay,
        PayCostKind::TapCreatures { .. } => InteractionIntentCode::Tap,
    }
}

/// Projection for the rule-sensitive board-selection family that the legacy
/// frontend currently derives in `getBoardChoiceView`. Keeping the frontend
/// path intact during this hidden phase avoids dual production authority while
/// making the engine contract complete for the later cutover.
fn selection_projection(
    waiting_for: &WaitingFor,
    state: &GameState,
    semantic_owner: PlayerId,
) -> Result<Option<SelectionProjection>, InteractionReasonCode> {
    let candidate_count = match waiting_for {
        WaitingFor::OpeningHandBottomCards { .. } | WaitingFor::MulliganDecision { .. } => state
            .players
            .get(semantic_owner.0 as usize)
            .map_or(0, |player| player.hand.len()),
        WaitingFor::EffectZoneChoice { cards, .. } => cards.len(),
        WaitingFor::KeepWithinTotalPowerChoice { eligible, .. }
        | WaitingFor::KeepExactPermanentsChoice { eligible, .. } => eligible.len(),
        WaitingFor::PayCost {
            kind:
                PayCostKind::RemoveCounter {
                    selection: CounterCostSelection::AmongObjects,
                    ..
                },
            ..
        } => 0,
        WaitingFor::PayCost { choices, .. } => choices.len(),
        WaitingFor::WardSacrificeChoice { permanents, .. }
        | WaitingFor::UnlessBounceChoice { permanents, .. } => permanents.len(),
        WaitingFor::CrewVehicle {
            eligible_creatures, ..
        }
        | WaitingFor::SaddleMount {
            eligible_creatures, ..
        }
        | WaitingFor::StationTarget {
            eligible_creatures, ..
        }
        | WaitingFor::HarmonizeTapChoice {
            eligible_creatures, ..
        } => eligible_creatures.len(),
        WaitingFor::BlightChoice { creatures, .. } => creatures.len(),
        WaitingFor::ChooseRingBearer { candidates, .. } => candidates.len(),
        WaitingFor::ChooseUntapSubset { group, .. } => group.len(),
        WaitingFor::ScryChoice { cards, .. }
        | WaitingFor::ArrangePlanarDeckTopChoice { cards, .. }
        | WaitingFor::SurveilChoice { cards, .. }
        | WaitingFor::SearchChoice { cards, .. }
        | WaitingFor::SearchPartitionChoice { cards, .. }
        | WaitingFor::ChooseFromZoneChoice { cards, .. }
        | WaitingFor::ConniveDiscard { cards, .. }
        | WaitingFor::DiscardChoice { cards, .. }
        | WaitingFor::DrawnThisTurnTopdeckChoice { cards, .. }
        | WaitingFor::DiscardToHandSize { cards, .. }
        | WaitingFor::WardDiscardChoice { cards, .. }
        | WaitingFor::CollectEvidenceChoice { cards, .. } => cards.len(),
        WaitingFor::DigChoice {
            selectable_cards, ..
        } => selectable_cards.len(),
        WaitingFor::SeparatePilesPartition { eligible, .. } => eligible.len(),
        _ => 0,
    };
    if candidate_count > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }

    Ok(match waiting_for {
        WaitingFor::OpeningHandBottomCards { pending, .. } => pending
            .iter()
            .find(|entry| entry.player == semantic_owner)
            .and_then(|entry| {
                state
                    .players
                    .get(semantic_owner.0 as usize)
                    .map(|player| SelectionProjection {
                        object_ids: player.hand.iter().copied().collect(),
                        constraint: count_constraint(entry.count as usize, entry.count as usize),
                        confirm: ConfirmSemantics::Explicit,
                        intent: InteractionIntentCode::Choose,
                        action: SelectionAction::SelectCards,
                        source_id: None,
                    })
            }),
        WaitingFor::MulliganDecision { pending, .. } => pending
            .iter()
            .find(|entry| entry.player == semantic_owner)
            .and_then(|entry| match entry.phase {
                crate::types::game_state::MulliganDecisionPhase::Declare => None,
                crate::types::game_state::MulliganDecisionPhase::BottomCards { count, then } => {
                    state
                        .players
                        .get(semantic_owner.0 as usize)
                        .map(|player| SelectionProjection {
                            object_ids: player
                                .hand
                                .iter()
                                .copied()
                                .filter(|object_id| {
                                    !matches!(
                                        then,
                                        crate::types::game_state::PendingMulliganAction::UseSerumPowder {
                                            object_id: powder_id,
                                        } if *object_id == powder_id
                                    )
                                })
                                .collect(),
                            constraint: count_constraint(count as usize, count as usize),
                            confirm: ConfirmSemantics::Explicit,
                            intent: InteractionIntentCode::Choose,
                            action: SelectionAction::SelectCards,
                            source_id: None,
                        })
                }
            }),
        WaitingFor::EffectZoneChoice {
            cards,
            count,
            min_count,
            up_to,
            effect_kind,
            destination,
            source_id,
            ..
        } => {
            let minimum = if *up_to { *min_count } else { *count };
            Some(SelectionProjection {
                object_ids: cards.clone(),
                constraint: count_constraint(minimum, *count),
                confirm: if minimum == 1 && *count == 1 {
                    ConfirmSemantics::Immediate
                } else {
                    ConfirmSemantics::Explicit
                },
                intent: effect_zone_intent(*effect_kind, *destination),
                action: SelectionAction::SelectCards,
                source_id: Some(*source_id),
            })
        }
        WaitingFor::KeepWithinTotalPowerChoice {
            eligible,
            cap,
            source_id,
            ..
        } => Some(SelectionProjection {
            object_ids: eligible.clone(),
            constraint: SelectionConstraint::Aggregate {
                function: InteractionAggregateFunction::Sum,
                property: InteractionObjectProperty::Power,
                comparator: AggregateComparator::AtMost,
                amount: *cap,
            },
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Keep,
            action: SelectionAction::KeepWithinPower,
            source_id: Some(*source_id),
        }),
        WaitingFor::KeepExactPermanentsChoice {
            eligible,
            required_count,
            source_id,
            ..
        } => Some(SelectionProjection {
            object_ids: eligible.clone(),
            constraint: count_constraint(*required_count, *required_count),
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Keep,
            action: SelectionAction::KeepExact,
            source_id: Some(*source_id),
        }),
        WaitingFor::PayCost {
            kind:
                PayCostKind::RemoveCounter {
                    selection: CounterCostSelection::AmongObjects,
                    ..
                },
            ..
        } => None,
        WaitingFor::PayCost {
            kind,
            choices,
            count,
            min_count,
            ..
        } => {
            let constraint = match kind {
                PayCostKind::TapCreatures {
                    aggregate: Some(aggregate),
                } => match aggregate.stat {
                    TapCreaturesAggregateStat::TotalPower => SelectionConstraint::Aggregate {
                        function: InteractionAggregateFunction::Sum,
                        property: InteractionObjectProperty::Power,
                        comparator: comparator_dto(aggregate.comparator),
                        amount: aggregate.value,
                    },
                },
                PayCostKind::ExileAggregate {
                    function,
                    property,
                    comparator,
                    value,
                    ..
                } => SelectionConstraint::Aggregate {
                    function: aggregate_function_code(*function),
                    property: object_property_code(*property),
                    comparator: comparator_dto(*comparator),
                    amount: *value,
                },
                PayCostKind::TapCreatures { aggregate: None }
                | PayCostKind::Discard
                | PayCostKind::Reveal
                | PayCostKind::Sacrifice
                | PayCostKind::ReturnToHand
                | PayCostKind::ExileFromZone { .. }
                | PayCostKind::ExileMaterials { .. }
                | PayCostKind::ExilePermanent { .. }
                | PayCostKind::ExileFromManaZone { .. }
                | PayCostKind::UnattachFrom { .. }
                | PayCostKind::RemoveCounter { .. }
                | PayCostKind::Behold { .. } => count_constraint(*min_count, *count),
            };
            Some(SelectionProjection {
                object_ids: choices.clone(),
                constraint,
                confirm: ConfirmSemantics::Explicit,
                intent: pay_cost_intent(kind),
                action: SelectionAction::SelectCards,
                source_id: None,
            })
        }
        WaitingFor::WardSacrificeChoice {
            permanents,
            min_total_power,
            ..
        } => Some(SelectionProjection {
            object_ids: permanents.clone(),
            constraint: min_total_power.map_or_else(
                || count_constraint(1, 1),
                |amount| SelectionConstraint::Aggregate {
                    function: InteractionAggregateFunction::Sum,
                    property: InteractionObjectProperty::Power,
                    comparator: AggregateComparator::AtLeast,
                    amount,
                },
            ),
            confirm: if min_total_power.is_none() {
                ConfirmSemantics::Immediate
            } else {
                ConfirmSemantics::Explicit
            },
            intent: InteractionIntentCode::Sacrifice,
            action: SelectionAction::SelectCards,
            source_id: None,
        }),
        WaitingFor::CrewVehicle {
            vehicle_id,
            crew_power,
            eligible_creatures,
            ..
        } => Some(SelectionProjection {
            object_ids: eligible_creatures.clone(),
            constraint: SelectionConstraint::Aggregate {
                function: InteractionAggregateFunction::Sum,
                property: InteractionObjectProperty::Power,
                comparator: AggregateComparator::AtLeast,
                amount: *crew_power as i32,
            },
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Crew,
            action: SelectionAction::Crew {
                vehicle_id: *vehicle_id,
            },
            source_id: Some(*vehicle_id),
        }),
        WaitingFor::SaddleMount {
            mount_id,
            saddle_power,
            eligible_creatures,
            ..
        } => Some(SelectionProjection {
            object_ids: eligible_creatures.clone(),
            constraint: SelectionConstraint::Aggregate {
                function: InteractionAggregateFunction::Sum,
                property: InteractionObjectProperty::Power,
                comparator: AggregateComparator::AtLeast,
                amount: *saddle_power as i32,
            },
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Saddle,
            action: SelectionAction::Saddle {
                mount_id: *mount_id,
            },
            source_id: Some(*mount_id),
        }),
        WaitingFor::StationTarget {
            spacecraft_id,
            eligible_creatures,
            ..
        } => Some(SelectionProjection {
            object_ids: eligible_creatures.clone(),
            constraint: count_constraint(1, 1),
            confirm: ConfirmSemantics::Immediate,
            intent: InteractionIntentCode::Station,
            action: SelectionAction::Station {
                spacecraft_id: *spacecraft_id,
            },
            source_id: Some(*spacecraft_id),
        }),
        WaitingFor::BlightChoice {
            creatures,
            pending_cast,
            ..
        } => Some(SelectionProjection {
            object_ids: creatures.clone(),
            constraint: count_constraint(1, 1),
            confirm: ConfirmSemantics::Immediate,
            intent: InteractionIntentCode::Blight,
            action: SelectionAction::SelectCards,
            source_id: Some(pending_cast.object_id),
        }),
        WaitingFor::UnlessBounceChoice { permanents, .. } => Some(SelectionProjection {
            object_ids: permanents.clone(),
            constraint: count_constraint(1, 1),
            confirm: ConfirmSemantics::Immediate,
            intent: InteractionIntentCode::Return,
            action: SelectionAction::SelectCards,
            source_id: None,
        }),
        WaitingFor::HarmonizeTapChoice {
            eligible_creatures,
            pending_cast,
            ..
        } => Some(SelectionProjection {
            object_ids: eligible_creatures.clone(),
            constraint: count_constraint(0, 1),
            confirm: ConfirmSemantics::Immediate,
            intent: InteractionIntentCode::Tap,
            action: SelectionAction::Harmonize,
            source_id: Some(pending_cast.object_id),
        }),
        WaitingFor::ChooseRingBearer { candidates, .. } => Some(SelectionProjection {
            object_ids: candidates.clone(),
            constraint: count_constraint(1, 1),
            confirm: ConfirmSemantics::Immediate,
            intent: InteractionIntentCode::RingBearer,
            action: SelectionAction::RingBearer,
            source_id: None,
        }),
        WaitingFor::ChooseUntapSubset { group, max, .. } => Some(SelectionProjection {
            object_ids: group.clone(),
            constraint: count_constraint(0, *max),
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Choose,
            action: SelectionAction::SelectCards,
            source_id: None,
        }),
        WaitingFor::ScryChoice { cards, .. } | WaitingFor::SurveilChoice { cards, .. } => {
            Some(SelectionProjection {
                object_ids: cards.clone(),
                constraint: count_constraint(0, cards.len()),
                confirm: ConfirmSemantics::Explicit,
                intent: InteractionIntentCode::Choose,
                action: SelectionAction::SelectCards,
                source_id: None,
            })
        }
        WaitingFor::ArrangePlanarDeckTopChoice {
            cards, keep_on_top, ..
        } => Some(SelectionProjection {
            object_ids: cards.clone(),
            constraint: count_constraint(*keep_on_top, *keep_on_top),
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Keep,
            action: SelectionAction::SelectCards,
            source_id: None,
        }),
        WaitingFor::DigChoice {
            selectable_cards,
            keep_count,
            up_to,
            source_id,
            ..
        } => {
            let maximum = (*keep_count).min(selectable_cards.len());
            Some(SelectionProjection {
                object_ids: selectable_cards.clone(),
                constraint: count_constraint(if *up_to { 0 } else { maximum }, maximum),
                confirm: ConfirmSemantics::Explicit,
                intent: InteractionIntentCode::Keep,
                action: SelectionAction::SelectCards,
                source_id: *source_id,
            })
        }
        WaitingFor::SearchChoice {
            cards,
            count,
            up_to,
            allows_partial_find,
            constraint,
            ..
        } => {
            let partial = *up_to || *allows_partial_find || constraint.permits_partial_find();
            let bounds = (if partial { 0 } else { *count }, *count);
            let constraint = match constraint {
                SearchSelectionConstraint::None => count_constraint(bounds.0, bounds.1),
                SearchSelectionConstraint::TotalManaValue { comparator, value } => {
                    SelectionConstraint::Aggregate {
                        function: InteractionAggregateFunction::Sum,
                        property: InteractionObjectProperty::ManaValue,
                        comparator: comparator_dto(*comparator),
                        amount: *value,
                    }
                }
                SearchSelectionConstraint::DistinctQualities { .. }
                | SearchSelectionConstraint::MatchEachFilter { .. } => {
                    SelectionConstraint::EngineValidatedCount {
                        min: bounds.0.min(u32::MAX as usize) as u32,
                        max: bounds.1.min(u32::MAX as usize) as u32,
                    }
                }
            };
            Some(SelectionProjection {
                object_ids: cards.clone(),
                constraint,
                confirm: ConfirmSemantics::Explicit,
                intent: InteractionIntentCode::Choose,
                action: SelectionAction::SelectCards,
                source_id: None,
            })
        }
        WaitingFor::SearchPartitionChoice {
            cards,
            primary_count,
            source_id,
            ..
        } => Some(SelectionProjection {
            object_ids: cards.clone(),
            constraint: count_constraint(*primary_count as usize, *primary_count as usize),
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Choose,
            action: SelectionAction::SelectCards,
            source_id: Some(*source_id),
        }),
        WaitingFor::ChooseFromZoneChoice {
            cards,
            count,
            up_to,
            constraint,
            source_id,
            ..
        } => Some(SelectionProjection {
            object_ids: cards.clone(),
            constraint: match constraint {
                None => count_constraint(if *up_to { 0 } else { *count }, *count),
                Some(ChooseFromZoneConstraint::DistinctCardTypes { .. }) => {
                    SelectionConstraint::EngineValidatedCount {
                        min: if *up_to { 0 } else { *count }.min(u32::MAX as usize) as u32,
                        max: (*count).min(u32::MAX as usize) as u32,
                    }
                }
            },
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Choose,
            action: SelectionAction::SelectCards,
            source_id: Some(*source_id),
        }),
        WaitingFor::ConniveDiscard {
            cards,
            count,
            source_id,
            ..
        }
        | WaitingFor::DiscardChoice {
            cards,
            count,
            source_id,
            up_to: false,
            unless_filter: None,
            ..
        } => Some(SelectionProjection {
            object_ids: cards.clone(),
            constraint: count_constraint(*count, *count),
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Choose,
            action: SelectionAction::SelectCards,
            source_id: Some(*source_id),
        }),
        WaitingFor::DiscardChoice {
            cards,
            count,
            source_id,
            up_to: true,
            unless_filter: None,
            ..
        } => Some(SelectionProjection {
            object_ids: cards.clone(),
            constraint: count_constraint(0, *count),
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Choose,
            action: SelectionAction::SelectCards,
            source_id: Some(*source_id),
        }),
        WaitingFor::DiscardChoice {
            cards,
            count,
            source_id,
            up_to,
            unless_filter: Some(_),
            ..
        } => Some(SelectionProjection {
            object_ids: cards.clone(),
            constraint: SelectionConstraint::EngineValidatedCount {
                min: if *up_to || *count == 0 { 0 } else { 1 },
                max: (*count).max(1).min(u32::MAX as usize) as u32,
            },
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Choose,
            action: SelectionAction::SelectCards,
            source_id: Some(*source_id),
        }),
        WaitingFor::DrawnThisTurnTopdeckChoice {
            cards,
            count,
            min_count,
            source_id,
            ..
        } => Some(SelectionProjection {
            object_ids: cards.clone(),
            constraint: count_constraint(*min_count, *count),
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Choose,
            action: SelectionAction::SelectCards,
            source_id: Some(*source_id),
        }),
        WaitingFor::DiscardToHandSize { cards, count, .. } => Some(SelectionProjection {
            object_ids: cards.clone(),
            constraint: count_constraint(*count, *count),
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Choose,
            action: SelectionAction::SelectCards,
            source_id: None,
        }),
        WaitingFor::WardDiscardChoice { cards, .. } => Some(SelectionProjection {
            object_ids: cards.clone(),
            constraint: count_constraint(1, 1),
            confirm: ConfirmSemantics::Immediate,
            intent: InteractionIntentCode::Pay,
            action: SelectionAction::SelectCards,
            source_id: None,
        }),
        WaitingFor::CollectEvidenceChoice {
            cards,
            minimum_mana_value,
            ..
        } => Some(SelectionProjection {
            object_ids: cards.clone(),
            constraint: SelectionConstraint::Aggregate {
                function: InteractionAggregateFunction::Sum,
                property: InteractionObjectProperty::ManaValue,
                comparator: AggregateComparator::AtLeast,
                amount: (*minimum_mana_value).min(i32::MAX as u32) as i32,
            },
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Pay,
            action: SelectionAction::SelectCards,
            source_id: None,
        }),
        WaitingFor::SeparatePilesPartition {
            eligible,
            source_id,
            ..
        } => Some(SelectionProjection {
            object_ids: eligible.iter().copied().collect(),
            constraint: count_constraint(0, eligible.len()),
            confirm: ConfirmSemantics::Explicit,
            intent: InteractionIntentCode::Choose,
            action: SelectionAction::PilePartition,
            source_id: Some(*source_id),
        }),
        WaitingFor::Priority { .. }
        | WaitingFor::MeldPairChoice { .. }
        | WaitingFor::MeldAttackTargetChoice { .. }
        | WaitingFor::ManaPayment { .. }
        | WaitingFor::AssistChoosePlayer { .. }
        | WaitingFor::AssistPayment { .. }
        | WaitingFor::ChooseXValue { .. }
        | WaitingFor::TargetSelection { .. }
        | WaitingFor::DeclareAttackers { .. }
        | WaitingFor::DeclareBlockers { .. }
        | WaitingFor::UntapChoice { .. }
        | WaitingFor::ExertChoice { .. }
        | WaitingFor::EnlistChoice { .. }
        | WaitingFor::GameOver { .. }
        | WaitingFor::ReplacementChoice { .. }
        | WaitingFor::OrderTriggers { .. }
        | WaitingFor::CopyTargetChoice { .. }
        | WaitingFor::ExploreChoice { .. }
        | WaitingFor::ReturnAsAuraTarget { .. }
        | WaitingFor::EquipTarget { .. }
        | WaitingFor::RedistributeLifeTotals { .. }
        | WaitingFor::CoinFlipKeepChoice { .. }
        | WaitingFor::RevealChoice { .. }
        | WaitingFor::OutsideGameChoice { .. }
        | WaitingFor::BeholdChoice { .. }
        | WaitingFor::ChooseOneOfBranch { .. }
        | WaitingFor::LearnChoice { .. }
        | WaitingFor::ManifestDreadChoice { .. }
        | WaitingFor::TriggerTargetSelection { .. }
        | WaitingFor::BetweenGamesSideboard { .. }
        | WaitingFor::BetweenGamesChoosePlayDraw { .. }
        | WaitingFor::NamedChoice { .. }
        | WaitingFor::OpponentGuess { .. }
        | WaitingFor::SpellbookDraft { .. }
        | WaitingFor::DamageSourceChoice { .. }
        | WaitingFor::ModeChoice { .. }
        | WaitingFor::OptionalCostChoice { .. }
        | WaitingFor::SpliceOffer { .. }
        | WaitingFor::DefilerPayment { .. }
        | WaitingFor::CastOffer { .. }
        | WaitingFor::ModalFaceChoice { .. }
        | WaitingFor::AlternativeCastChoice { .. }
        | WaitingFor::MutateMergeChoice { .. }
        | WaitingFor::CipherEncodeChoice { .. }
        | WaitingFor::CastingVariantChoice { .. }
        | WaitingFor::ChoosePermanentTypeSlot { .. }
        | WaitingFor::MultiTargetSelection { .. }
        | WaitingFor::AbilityModeChoice { .. }
        | WaitingFor::OptionalEffectChoice { .. }
        | WaitingFor::PairChoice { .. }
        | WaitingFor::TributeChoice { .. }
        | WaitingFor::MiracleReveal { .. }
        | WaitingFor::OpponentMayChoice { .. }
        | WaitingFor::LoopShortcut { .. }
        | WaitingFor::RespondToShortcut { .. }
        | WaitingFor::PrecastCopyShortcutOffer { .. }
        | WaitingFor::RespondToPrecastCopyShortcut { .. }
        | WaitingFor::UnlessPayment { .. }
        | WaitingFor::UnlessPaymentChooseCost { .. }
        | WaitingFor::ChooseRoomDoor { .. }
        | WaitingFor::ChooseDungeon { .. }
        | WaitingFor::ChooseDungeonRoom { .. }
        | WaitingFor::SpecializeColor { .. }
        | WaitingFor::ActivationCostOneOfChoice { .. }
        | WaitingFor::CostTypeChoice { .. }
        | WaitingFor::PayManaAbilityMana { .. }
        | WaitingFor::ChooseManaColor { .. }
        | WaitingFor::RevealUntilKeptChoice { .. }
        | WaitingFor::RepeatDecision { .. }
        | WaitingFor::TopOrBottomChoice { .. }
        | WaitingFor::PopulateChoice { .. }
        | WaitingFor::ClashChooseOpponent { .. }
        | WaitingFor::ChooseFromZoneOpponentChooser { .. }
        | WaitingFor::ChooseAnnouncingOpponent { .. }
        | WaitingFor::ClashCardPlacement { .. }
        | WaitingFor::VoteChoice { .. }
        | WaitingFor::SeparatePilesChooseOpponent { .. }
        | WaitingFor::SeparatePilesChoice { .. }
        | WaitingFor::CompanionReveal { .. }
        | WaitingFor::ChooseLegend { .. }
        | WaitingFor::CommanderZoneChoice { .. }
        | WaitingFor::BattleProtectorChoice { .. }
        | WaitingFor::ProliferateChoice { .. }
        | WaitingFor::TimeTravelChoice { .. }
        | WaitingFor::ChooseObjectsSelection { .. }
        | WaitingFor::CategoryChoice { .. }
        | WaitingFor::EachPlayerCopyChosenSelection { .. }
        | WaitingFor::CopyRetarget { .. }
        | WaitingFor::AssignCombatDamage { .. }
        | WaitingFor::AssignBlockerDamage { .. }
        | WaitingFor::DistributeAmong { .. }
        | WaitingFor::MoveCountersDistribution { .. }
        | WaitingFor::RemoveCountersChoice { .. }
        | WaitingFor::PayAmountChoice { .. }
        | WaitingFor::RetargetChoice { .. }
        | WaitingFor::CombatTaxPayment { .. }
        | WaitingFor::PhyrexianPayment { .. } => None,
    })
}

fn counter_distribution_projection(
    waiting_for: &WaitingFor,
    state: &GameState,
) -> Result<Option<CounterDistributionProjection>, InteractionReasonCode> {
    let WaitingFor::PayCost {
        kind:
            PayCostKind::RemoveCounter {
                counter_type,
                count,
                selection: CounterCostSelection::AmongObjects,
            },
        choices,
        ..
    } = waiting_for
    else {
        return Ok(None);
    };

    if choices.len() > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }

    let mut candidates = Vec::new();
    for object_id in choices {
        let Some(object) = state.objects.get(object_id) else {
            continue;
        };
        let remaining = MAX_INTERACTION_LIST_LEN - candidates.len();
        let available_counter_kinds = match counter_type {
            CounterMatch::Any => object
                .counters
                .values()
                .filter(|available| **available > 0)
                .take(remaining + 1)
                .count(),
            CounterMatch::OfType(expected) => usize::from(
                object
                    .counters
                    .get(expected)
                    .is_some_and(|available| *available > 0),
            ),
        };
        if available_counter_kinds > remaining {
            return Err(InteractionReasonCode::PayloadTooLarge);
        }
        let mut counters: Vec<_> = match counter_type {
            CounterMatch::Any => object
                .counters
                .iter()
                .filter(|(_, available)| **available > 0)
                .map(|(counter_type, available)| (counter_type.clone(), *available))
                .collect(),
            CounterMatch::OfType(expected) => object
                .counters
                .get(expected)
                .copied()
                .filter(|available| *available > 0)
                .map(|available| vec![(expected.clone(), available)])
                .unwrap_or_default(),
        };
        counters.sort_by(|a, b| a.0.cmp(&b.0));
        candidates.extend(counters.into_iter().map(|(counter_type, available)| {
            CounterAssignmentCandidate {
                object_id: *object_id,
                counter_type,
                available,
            }
        }));
    }
    Ok(Some(CounterDistributionProjection {
        candidates,
        total: *count,
    }))
}

fn interaction_choice_id(
    interaction_id: &InteractionId,
    namespace: char,
    index: usize,
) -> InteractionChoiceId {
    InteractionChoiceId(format!("{}.{}{}", interaction_id.0, namespace, index))
}

fn zone_code(zone: Zone) -> InteractionZoneCode {
    match zone {
        Zone::Battlefield => InteractionZoneCode::Battlefield,
        Zone::Hand => InteractionZoneCode::Hand,
        Zone::Library => InteractionZoneCode::Library,
        Zone::Graveyard => InteractionZoneCode::Graveyard,
        Zone::Stack => InteractionZoneCode::Stack,
        Zone::Exile => InteractionZoneCode::Exile,
        Zone::Command => InteractionZoneCode::Command,
    }
}

#[derive(Debug, Clone, Copy)]
struct SurfaceRole {
    code: InteractionRoleCode,
    index: Option<u32>,
}

impl SurfaceRole {
    fn indexed(code: InteractionRoleCode, index: usize) -> Self {
        Self {
            code,
            index: Some(index.min(u32::MAX as usize) as u32),
        }
    }
}

impl From<InteractionRoleCode> for SurfaceRole {
    fn from(code: InteractionRoleCode) -> Self {
        Self { code, index: None }
    }
}

fn object_surface(
    state: &GameState,
    object_id: ObjectId,
    role: impl Into<SurfaceRole>,
) -> Option<InteractionPresentationSurface> {
    if !visibility::interaction_object_identity_is_visible(state, object_id) {
        return None;
    }
    let object = state.objects.get(&object_id)?;
    let role = role.into();
    Some(InteractionPresentationSurface::Object {
        role: role.code,
        index: role.index,
        reference: object_id.0.to_string(),
        name: Some(object.name.clone()),
        zone: Some(zone_code(object.zone)),
        controller: Some(object.controller.0),
        power: object.power,
        tapped: Some(object.tapped),
    })
}

fn push_object_surface(
    surfaces: &mut Vec<InteractionPresentationSurface>,
    state: &GameState,
    object_id: ObjectId,
    role: impl Into<SurfaceRole>,
) {
    if let Some(surface) = object_surface(state, object_id, role) {
        surfaces.push(surface);
    }
}

fn push_player_surface(
    surfaces: &mut Vec<InteractionPresentationSurface>,
    player: PlayerId,
    role: impl Into<SurfaceRole>,
) {
    let role = role.into();
    surfaces.push(InteractionPresentationSurface::Player {
        role: role.code,
        index: role.index,
        seat: player.0,
    });
}

fn push_value_surface(
    surfaces: &mut Vec<InteractionPresentationSurface>,
    role: impl Into<SurfaceRole>,
    value: impl ToString,
) {
    let role = role.into();
    surfaces.push(InteractionPresentationSurface::Value {
        role: role.code,
        index: role.index,
        value: value.to_string(),
    });
}

fn mana_type_code(mana_type: ManaType) -> &'static str {
    match mana_type {
        ManaType::White => "W",
        ManaType::Blue => "U",
        ManaType::Black => "B",
        ManaType::Red => "R",
        ManaType::Green => "G",
        ManaType::Colorless => "C",
    }
}

fn mana_color_code(color: ManaColor) -> &'static str {
    match color {
        ManaColor::White => "white",
        ManaColor::Blue => "blue",
        ManaColor::Black => "black",
        ManaColor::Red => "red",
        ManaColor::Green => "green",
    }
}

fn mana_color_symbol(color: ManaColor) -> &'static str {
    match color {
        ManaColor::White => "W",
        ManaColor::Blue => "U",
        ManaColor::Black => "B",
        ManaColor::Red => "R",
        ManaColor::Green => "G",
    }
}

fn aggregate_function_code(function: AggregateFunction) -> InteractionAggregateFunction {
    match function {
        AggregateFunction::Max => InteractionAggregateFunction::Max,
        AggregateFunction::Min => InteractionAggregateFunction::Min,
        AggregateFunction::Sum => InteractionAggregateFunction::Sum,
    }
}

fn mana_color_dto(color: ManaColor) -> InteractionManaColor {
    match color {
        ManaColor::White => InteractionManaColor::White,
        ManaColor::Blue => InteractionManaColor::Blue,
        ManaColor::Black => InteractionManaColor::Black,
        ManaColor::Red => InteractionManaColor::Red,
        ManaColor::Green => InteractionManaColor::Green,
    }
}

fn object_property_code(property: ObjectProperty) -> InteractionObjectProperty {
    match property {
        ObjectProperty::Power => InteractionObjectProperty::Power,
        ObjectProperty::Toughness => InteractionObjectProperty::Toughness,
        ObjectProperty::ManaValue => InteractionObjectProperty::ManaValue,
        ObjectProperty::ManaSymbolCount(color) => InteractionObjectProperty::ManaSymbolCount {
            color: mana_color_dto(color),
        },
    }
}

fn cast_payment_mode_code(mode: CastPaymentMode) -> &'static str {
    match mode {
        CastPaymentMode::Auto => "auto",
        CastPaymentMode::Manual => "manual",
    }
}

fn pile_side_code(side: PileSide) -> &'static str {
    match side {
        PileSide::A => "a",
        PileSide::B => "b",
    }
}

fn alternative_cast_code(choice: AlternativeCastDecision) -> &'static str {
    match choice {
        AlternativeCastDecision::Normal => "normal",
        AlternativeCastDecision::Alternative => "alternative",
    }
}

fn core_type_code(core_type: CoreType) -> &'static str {
    match core_type {
        CoreType::Artifact => "artifact",
        CoreType::Creature => "creature",
        CoreType::Enchantment => "enchantment",
        CoreType::Instant => "instant",
        CoreType::Land => "land",
        CoreType::Planeswalker => "planeswalker",
        CoreType::Sorcery => "sorcery",
        CoreType::Tribal => "tribal",
        CoreType::Battle => "battle",
        CoreType::Kindred => "kindred",
        CoreType::Dungeon => "dungeon",
        CoreType::Plane => "plane",
        CoreType::Phenomenon => "phenomenon",
        CoreType::Scheme => "scheme",
        CoreType::Conspiracy => "conspiracy",
    }
}

fn auto_may_choice_code(choice: AutoMayChoice) -> &'static str {
    match choice {
        AutoMayChoice::Accept => "accept",
        AutoMayChoice::Decline => "decline",
    }
}

fn dungeon_code(dungeon: DungeonId) -> &'static str {
    match dungeon {
        DungeonId::LostMineOfPhandelver => "lostMineOfPhandelver",
        DungeonId::DungeonOfTheMadMage => "dungeonOfTheMadMage",
        DungeonId::TombOfAnnihilation => "tombOfAnnihilation",
        DungeonId::Undercity => "undercity",
        DungeonId::BaldursGateWilderness => "baldursGateWilderness",
    }
}

fn room_door_code(door: RoomDoor) -> &'static str {
    match door {
        RoomDoor::Left => "left",
        RoomDoor::Right => "right",
    }
}

fn door_lock_op_code(op: DoorLockOp) -> &'static str {
    match op {
        DoorLockOp::Unlock => "unlock",
        DoorLockOp::Lock => "lock",
        DoorLockOp::LockOrUnlock => "lockOrUnlock",
    }
}

fn cast_choice_code(choice: CastChoice) -> &'static str {
    match choice {
        CastChoice::Cast => "cast",
        CastChoice::Decline => "decline",
    }
}

fn merge_side_code(side: MergeSide) -> &'static str {
    match side {
        MergeSide::Top => "top",
        MergeSide::Bottom => "bottom",
    }
}

fn combat_damage_assignment_mode_code(mode: CombatDamageAssignmentMode) -> &'static str {
    match mode {
        CombatDamageAssignmentMode::Normal => "normal",
        CombatDamageAssignmentMode::AsThoughUnblocked => "asThoughUnblocked",
    }
}

fn shard_choice_code(choice: ShardChoice) -> &'static str {
    match choice {
        ShardChoice::PayMana => "mana",
        ShardChoice::PayLife => "life",
    }
}

fn mana_cost_symbols(cost: &ManaCost) -> Vec<String> {
    match cost {
        ManaCost::NoCost => vec!["NoCost".to_string()],
        ManaCost::Cost { shards, generic } => {
            let mut symbols = Vec::with_capacity(shards.len() + usize::from(*generic > 0));
            if *generic > 0 {
                symbols.push(generic.to_string());
            }
            symbols.extend(shards.iter().map(|shard| shard.symbol().to_string()));
            symbols
        }
        ManaCost::SelfManaCost => vec!["SelfManaCost".to_string()],
        ManaCost::SelfManaValue => vec!["SelfManaValue".to_string()],
        ManaCost::SelfManaCostReduced { reduction } => {
            vec![format!("SelfManaCostReduced:{reduction}")]
        }
    }
}

fn project_casting_variant(
    variant: CastingVariant,
    state: &GameState,
    surfaces: &mut Vec<InteractionPresentationSurface>,
) {
    let variant_code = match variant {
        CastingVariant::Normal => "Normal",
        CastingVariant::Adventure => "Adventure",
        CastingVariant::Omen => "Omen",
        CastingVariant::Warp => "Warp",
        CastingVariant::Escape => "Escape",
        CastingVariant::Retrace => "Retrace",
        CastingVariant::Harmonize => "Harmonize",
        CastingVariant::Mayhem => "Mayhem",
        CastingVariant::Flashback => "Flashback",
        CastingVariant::Aftermath => "Aftermath",
        CastingVariant::Disturb => "Disturb",
        CastingVariant::GraveyardPermission { source, .. } => {
            push_object_surface(
                surfaces,
                state,
                source,
                InteractionRoleCode::PermissionSource,
            );
            "GraveyardPermission"
        }
        CastingVariant::HandPermission { source, .. } => {
            push_object_surface(
                surfaces,
                state,
                source,
                InteractionRoleCode::PermissionSource,
            );
            "HandPermission"
        }
        CastingVariant::ExilePermission { source, .. } => {
            push_object_surface(
                surfaces,
                state,
                source,
                InteractionRoleCode::PermissionSource,
            );
            "ExilePermission"
        }
        CastingVariant::Sneak {
            returned_creature,
            placement,
        } => {
            push_object_surface(
                surfaces,
                state,
                returned_creature,
                InteractionRoleCode::ReturnCreature,
            );
            if let Some(placement) = placement {
                push_player_surface(surfaces, placement.defender, InteractionRoleCode::Defender);
                push_attack_target_surface(
                    surfaces,
                    state,
                    &placement.attack_target,
                    InteractionRoleCode::AttackTarget,
                );
            }
            "Sneak"
        }
        CastingVariant::WebSlinging { returned_creature } => {
            push_object_surface(
                surfaces,
                state,
                returned_creature,
                InteractionRoleCode::ReturnCreature,
            );
            "WebSlinging"
        }
        CastingVariant::Miracle => "Miracle",
        CastingVariant::Madness => "Madness",
        CastingVariant::Evoke => "Evoke",
        CastingVariant::Emerge => "Emerge",
        CastingVariant::Dash => "Dash",
        CastingVariant::Blitz => "Blitz",
        CastingVariant::Spectacle => "Spectacle",
        CastingVariant::Suspend => "Suspend",
        CastingVariant::Plot => "Plot",
        CastingVariant::Foretell => "Foretell",
        CastingVariant::Overload => "Overload",
        CastingVariant::Bestow => "Bestow",
        CastingVariant::Awaken => "Awaken",
        CastingVariant::Cleave => "Cleave",
        CastingVariant::MoreThanMeetsTheEye => "MoreThanMeetsTheEye",
        CastingVariant::Impending => "Impending",
        CastingVariant::Prototype => "Prototype",
        CastingVariant::Mutate => "Mutate",
        CastingVariant::Freerunning => "Freerunning",
        CastingVariant::Prowl => "Prowl",
        CastingVariant::JumpStart => "JumpStart",
        CastingVariant::Fuse => "Fuse",
        CastingVariant::Surge => "Surge",
        CastingVariant::FaceDown => "FaceDown",
    };
    push_value_surface(surfaces, InteractionRoleCode::CastingVariant, variant_code);
}

fn push_target_surface(
    surfaces: &mut Vec<InteractionPresentationSurface>,
    state: &GameState,
    target: &TargetRef,
    role: SurfaceRole,
) {
    match target {
        TargetRef::Object(object_id) => push_object_surface(surfaces, state, *object_id, role),
        TargetRef::Player(player) => push_player_surface(surfaces, *player, role),
    }
}

fn push_attack_target_surface(
    surfaces: &mut Vec<InteractionPresentationSurface>,
    state: &GameState,
    target: &AttackTarget,
    role: impl Into<SurfaceRole>,
) {
    let role = role.into();
    match target {
        AttackTarget::Player(player) => push_player_surface(surfaces, *player, role),
        AttackTarget::Planeswalker(object_id) | AttackTarget::Battle(object_id) => {
            push_object_surface(surfaces, state, *object_id, role)
        }
    }
}

fn push_object_list(
    surfaces: &mut Vec<InteractionPresentationSurface>,
    state: &GameState,
    object_ids: &[ObjectId],
    role: InteractionRoleCode,
) {
    for (index, object_id) in object_ids.iter().enumerate() {
        push_object_surface(
            surfaces,
            state,
            *object_id,
            SurfaceRole::indexed(role, index),
        );
    }
}

/// Exhaustive, viewer-filtered projection of the fields that distinguish one
/// exact action candidate from its siblings. This is intentionally action
/// aware: adding a `GameAction` variant is a compile-time obligation here.
fn project_action_payload(
    action: &GameAction,
    state: &GameState,
    surfaces: &mut Vec<InteractionPresentationSurface>,
) {
    match action {
        GameAction::PassPriority
        | GameAction::CancelCast
        | GameAction::KeepAllCopyTargets
        | GameAction::RollPlanarDie
        | GameAction::CompanionToHand
        | GameAction::CancelAutoPass
        | GameAction::PassParadigmOffer => {}
        GameAction::ChooseMeldPair { partner_id, .. } => {
            push_object_surface(surfaces, state, *partner_id, InteractionRoleCode::Partner)
        }
        GameAction::ChooseEntryAttackTarget { target } => {
            push_attack_target_surface(surfaces, state, target, InteractionRoleCode::AttackTarget)
        }
        GameAction::PlayLand { .. }
        | GameAction::Foretell { .. }
        | GameAction::TapLandForMana { .. }
        | GameAction::UntapLandForMana { .. }
        | GameAction::Transform { .. }
        | GameAction::PlayFaceDown { .. }
        | GameAction::CastPreparedCopy { .. }
        | GameAction::CastParadigmCopy { .. } => {}
        GameAction::CastSpell {
            targets,
            payment_mode,
            ..
        } => {
            push_object_list(surfaces, state, targets, InteractionRoleCode::Target);
            push_value_surface(
                surfaces,
                InteractionRoleCode::PaymentMode,
                cast_payment_mode_code(*payment_mode),
            );
        }
        GameAction::ActivateAbility { ability_index, .. } => {
            push_value_surface(surfaces, InteractionRoleCode::AbilityIndex, ability_index)
        }
        GameAction::DeclareAttackers { attacks, bands } => {
            for (index, (attacker, target)) in attacks.iter().enumerate() {
                push_object_surface(
                    surfaces,
                    state,
                    *attacker,
                    SurfaceRole::indexed(InteractionRoleCode::Attacker, index),
                );
                push_attack_target_surface(
                    surfaces,
                    state,
                    target,
                    SurfaceRole::indexed(InteractionRoleCode::AttackTarget, index),
                );
            }
            push_value_surface(surfaces, InteractionRoleCode::BandCount, bands.len());
        }
        GameAction::DeclareBlockers { assignments } => {
            for (index, (blocker, attacker)) in assignments.iter().enumerate() {
                push_object_surface(
                    surfaces,
                    state,
                    *blocker,
                    SurfaceRole::indexed(InteractionRoleCode::Blocker, index),
                );
                push_object_surface(
                    surfaces,
                    state,
                    *attacker,
                    SurfaceRole::indexed(InteractionRoleCode::Blocked, index),
                );
            }
        }
        GameAction::ChooseUntap { untap, .. } => {
            push_value_surface(surfaces, InteractionRoleCode::Untap, untap)
        }
        GameAction::ChooseExert { exert } => {
            push_value_surface(surfaces, InteractionRoleCode::Exert, exert)
        }
        GameAction::ChooseEnlist { target } => {
            if let Some(target) = target {
                push_object_surface(surfaces, state, *target, InteractionRoleCode::EnlistTarget);
            } else {
                push_value_surface(surfaces, InteractionRoleCode::Enlist, "decline");
            }
        }
        GameAction::ChooseClashOpponent { opponent }
        | GameAction::ChooseZoneOpponentChooser { opponent }
        | GameAction::ChoosePileOpponent { opponent }
        | GameAction::ChooseAnnouncingOpponent { opponent } => {
            push_player_surface(surfaces, *opponent, InteractionRoleCode::Opponent)
        }
        GameAction::ChooseAssistPlayer { player } => {
            if let Some(player) = player {
                push_player_surface(surfaces, *player, InteractionRoleCode::AssistPlayer);
            } else {
                push_value_surface(surfaces, InteractionRoleCode::Assist, "decline");
            }
        }
        GameAction::CommitAssistPayment { generic } => {
            push_value_surface(surfaces, InteractionRoleCode::GenericMana, generic)
        }
        GameAction::MulliganDecision { choice } => match choice {
            MulliganChoice::Keep => {
                push_value_surface(surfaces, InteractionRoleCode::Mulligan, "keep")
            }
            MulliganChoice::Mulligan => {
                push_value_surface(surfaces, InteractionRoleCode::Mulligan, "mulligan")
            }
            MulliganChoice::UseSerumPowder { object_id } => {
                push_value_surface(surfaces, InteractionRoleCode::Mulligan, "serumPowder");
                push_object_surface(
                    surfaces,
                    state,
                    *object_id,
                    InteractionRoleCode::SerumPowder,
                );
            }
        },
        GameAction::ReorderHand { order } => {
            push_object_list(surfaces, state, order, InteractionRoleCode::HandCard)
        }
        GameAction::SpendPoolMana { .. } | GameAction::UnspendPoolMana { .. } => {}
        GameAction::SelectCards { cards } => {
            push_object_list(surfaces, state, cards, InteractionRoleCode::Selected)
        }
        GameAction::ChooseRemoveCounterCostDistribution { distribution } => {
            for (index, choice) in distribution.iter().enumerate() {
                push_object_surface(
                    surfaces,
                    state,
                    choice.object_id,
                    SurfaceRole::indexed(InteractionRoleCode::CounterSource, index),
                );
                push_value_surface(
                    surfaces,
                    SurfaceRole::indexed(InteractionRoleCode::CounterType, index),
                    choice.counter_type.as_str(),
                );
                push_value_surface(
                    surfaces,
                    SurfaceRole::indexed(InteractionRoleCode::Amount, index),
                    choice.count,
                );
            }
        }
        GameAction::SelectCoinFlips { keep_indices } => {
            for index in keep_indices {
                push_value_surface(surfaces, InteractionRoleCode::CoinFlipIndex, index);
            }
        }
        GameAction::ChooseOutsideGameCards { selections } => {
            for selection in selections {
                match selection {
                    OutsideGameSelection::Sideboard { sideboard_index } => push_value_surface(
                        surfaces,
                        InteractionRoleCode::SideboardIndex,
                        sideboard_index,
                    ),
                    OutsideGameSelection::FaceUpExile { object_id } => push_object_surface(
                        surfaces,
                        state,
                        *object_id,
                        InteractionRoleCode::FaceUpExile,
                    ),
                }
            }
        }
        GameAction::SelectTargets { targets } => {
            for (index, target) in targets.iter().enumerate() {
                push_target_surface(
                    surfaces,
                    state,
                    target,
                    SurfaceRole::indexed(InteractionRoleCode::Target, index),
                );
            }
        }
        GameAction::ChooseTarget { target } => {
            if let Some(target) = target {
                push_target_surface(surfaces, state, target, InteractionRoleCode::Target.into());
            } else {
                push_value_surface(surfaces, InteractionRoleCode::Target, "none");
            }
        }
        GameAction::ChooseReplacement { index }
        | GameAction::ChooseBranch { index }
        | GameAction::ChooseCastingVariant { index }
        | GameAction::ChooseActivationCostBranch { index } => {
            push_value_surface(surfaces, InteractionRoleCode::OptionIndex, index)
        }
        GameAction::OrderTriggers { order } => {
            for index in order {
                push_value_surface(surfaces, InteractionRoleCode::TriggerIndex, index);
            }
        }
        GameAction::Equip { target_id, .. } => {
            push_object_surface(surfaces, state, *target_id, InteractionRoleCode::Target)
        }
        GameAction::CrewVehicle { creature_ids, .. }
        | GameAction::SaddleMount { creature_ids, .. } => push_object_list(
            surfaces,
            state,
            creature_ids,
            InteractionRoleCode::CrewMember,
        ),
        GameAction::ActivateStation { creature_id, .. } => {
            if let Some(creature_id) = creature_id {
                push_object_surface(
                    surfaces,
                    state,
                    *creature_id,
                    InteractionRoleCode::StationCrew,
                )
            }
        }
        GameAction::TurnFaceUp { x, .. } => push_value_surface(surfaces, InteractionRoleCode::X, x),
        GameAction::SubmitSideboard { main, sideboard } => {
            for card in main {
                push_value_surface(
                    surfaces,
                    InteractionRoleCode::MainCard,
                    format!("{}:{}", card.name, card.count),
                );
            }
            for card in sideboard {
                push_value_surface(
                    surfaces,
                    InteractionRoleCode::SideboardCard,
                    format!("{}:{}", card.name, card.count),
                );
            }
        }
        GameAction::ChoosePlayDraw { play_first } => {
            push_value_surface(surfaces, InteractionRoleCode::PlayFirst, play_first)
        }
        GameAction::ChooseOption { choice } => {
            push_value_surface(surfaces, InteractionRoleCode::Option, choice)
        }
        GameAction::SubmitVoteCandidate { candidate_index } => push_value_surface(
            surfaces,
            InteractionRoleCode::CandidateIndex,
            candidate_index,
        ),
        GameAction::SubmitSpellbookDraft { card } => {
            push_value_surface(surfaces, InteractionRoleCode::CardName, card)
        }
        GameAction::SubmitPilePartition { pile_a } => {
            push_object_list(surfaces, state, pile_a, InteractionRoleCode::PileA)
        }
        GameAction::ChoosePile { pile } => {
            push_value_surface(surfaces, InteractionRoleCode::Pile, pile_side_code(*pile))
        }
        GameAction::SubmitLifeRedistribution { option_index } => {
            push_value_surface(surfaces, InteractionRoleCode::OptionIndex, option_index)
        }
        GameAction::ChooseDamageSource { .. } => {}
        GameAction::SelectModes { indices } => {
            for index in indices {
                push_value_surface(surfaces, InteractionRoleCode::ModeIndex, index);
            }
        }
        GameAction::DecideOptionalCost { pay } | GameAction::PayUnlessCost { pay } => {
            push_value_surface(surfaces, InteractionRoleCode::Pay, pay)
        }
        GameAction::ChooseAdventureFace { creature } => push_value_surface(
            surfaces,
            InteractionRoleCode::Face,
            if *creature { "creature" } else { "adventure" },
        ),
        GameAction::ChooseModalFace { back_face } => push_value_surface(
            surfaces,
            InteractionRoleCode::Face,
            if *back_face { "back" } else { "front" },
        ),
        GameAction::ChooseAlternativeCast { choice } => push_value_surface(
            surfaces,
            InteractionRoleCode::CastCost,
            alternative_cast_code(*choice),
        ),
        GameAction::ChoosePermanentTypeSlot { slot } => push_value_surface(
            surfaces,
            InteractionRoleCode::PermanentType,
            core_type_code(*slot),
        ),
        GameAction::ActivateNinjutsu {
            creature_to_return, ..
        } => push_object_surface(
            surfaces,
            state,
            *creature_to_return,
            InteractionRoleCode::ReturnCreature,
        ),
        GameAction::CastSpellAsSneak {
            creature_to_return,
            payment_mode,
            ..
        }
        | GameAction::CastSpellAsWebSlinging {
            creature_to_return,
            payment_mode,
            ..
        } => {
            push_object_surface(
                surfaces,
                state,
                *creature_to_return,
                InteractionRoleCode::ReturnCreature,
            );
            push_value_surface(
                surfaces,
                InteractionRoleCode::PaymentMode,
                cast_payment_mode_code(*payment_mode),
            );
        }
        GameAction::CastSpellForFree {
            source_id,
            payment_mode,
            ..
        } => {
            push_object_surface(
                surfaces,
                state,
                *source_id,
                InteractionRoleCode::PermissionSource,
            );
            push_value_surface(
                surfaces,
                InteractionRoleCode::PaymentMode,
                cast_payment_mode_code(*payment_mode),
            );
        }
        GameAction::CastSpellAsMiracle { payment_mode, .. }
        | GameAction::CastSpellAsMadness { payment_mode, .. } => push_value_surface(
            surfaces,
            InteractionRoleCode::PaymentMode,
            cast_payment_mode_code(*payment_mode),
        ),
        GameAction::DecideOptionalEffect { accept } => {
            push_value_surface(surfaces, InteractionRoleCode::Accept, accept)
        }
        GameAction::RespondToSpliceOffer { card } => {
            if let Some(card) = card {
                push_object_surface(surfaces, state, *card, InteractionRoleCode::SpliceCard);
            } else {
                push_value_surface(surfaces, InteractionRoleCode::Splice, "decline");
            }
        }
        GameAction::DecideOptionalEffectAndRemember { choice } => push_value_surface(
            surfaces,
            InteractionRoleCode::Choice,
            auto_may_choice_code(*choice),
        ),
        GameAction::ChooseUnlessCostBranch { choice } => match choice {
            UnlessCostBranch::Decline => {
                push_value_surface(surfaces, InteractionRoleCode::CostBranch, "decline")
            }
            UnlessCostBranch::Pay { index } => {
                push_value_surface(surfaces, InteractionRoleCode::CostBranchIndex, index)
            }
        },
        GameAction::PayCombatTax { accept } => {
            push_value_surface(surfaces, InteractionRoleCode::Accept, accept)
        }
        GameAction::ChooseRingBearer { .. } => {}
        GameAction::ChoosePair { partner } => {
            if let Some(partner) = partner {
                push_object_surface(surfaces, state, *partner, InteractionRoleCode::Partner)
            } else {
                push_value_surface(surfaces, InteractionRoleCode::Pair, "decline");
            }
        }
        GameAction::ChooseDungeon { dungeon } => push_value_surface(
            surfaces,
            InteractionRoleCode::Dungeon,
            dungeon_code(*dungeon),
        ),
        GameAction::ChooseDungeonRoom { room_index } => {
            push_value_surface(surfaces, InteractionRoleCode::RoomIndex, room_index)
        }
        GameAction::UnlockRoomDoor { door, .. } => {
            push_value_surface(surfaces, InteractionRoleCode::Door, room_door_code(*door))
        }
        GameAction::ChooseRoomDoor { op, door, .. } => {
            push_value_surface(
                surfaces,
                InteractionRoleCode::Operation,
                door_lock_op_code(*op),
            );
            push_value_surface(surfaces, InteractionRoleCode::Door, room_door_code(*door));
        }
        GameAction::TapForConvoke { mana_type, .. } => {
            surfaces.push(InteractionPresentationSurface::Mana {
                role: InteractionRoleCode::ConvokeMana,
                index: None,
                symbols: vec![mana_type_code(*mana_type).to_string()],
            });
        }
        GameAction::HarmonizeTap { creature_id } => {
            if let Some(creature_id) = creature_id {
                push_object_surface(
                    surfaces,
                    state,
                    *creature_id,
                    InteractionRoleCode::HarmonizeCreature,
                )
            } else {
                push_value_surface(surfaces, InteractionRoleCode::Harmonize, "decline");
            }
        }
        GameAction::DeclareCompanion { choice } => match choice {
            crate::types::game_state::CompanionDeclaration::Reveal(reveal) => {
                push_value_surface(surfaces, InteractionRoleCode::Companion, &reveal.name)
            }
            crate::types::game_state::CompanionDeclaration::Decline => {
                push_value_surface(surfaces, InteractionRoleCode::Companion, "decline")
            }
        },
        GameAction::DiscoverChoice { choice }
        | GameAction::GraveyardPaidCastChoice { choice }
        | GameAction::CascadeChoice { choice }
        | GameAction::RippleChoice { choice } => push_value_surface(
            surfaces,
            InteractionRoleCode::CastChoice,
            cast_choice_code(*choice),
        ),
        GameAction::FreeCastWindowChoice { selection } => {
            if let Some(selection) = selection {
                push_object_surface(surfaces, state, *selection, InteractionRoleCode::CastCard)
            } else {
                push_value_surface(surfaces, InteractionRoleCode::CastChoice, "decline");
            }
        }
        GameAction::ChooseTopOrBottom { top } => push_value_surface(
            surfaces,
            InteractionRoleCode::Placement,
            if *top { "top" } else { "bottom" },
        ),
        GameAction::ChooseMutateMergeSide { side } => push_value_surface(
            surfaces,
            InteractionRoleCode::MergeSide,
            merge_side_code(*side),
        ),
        GameAction::CipherEncode { creature } => {
            if let Some(creature) = creature {
                push_object_surface(
                    surfaces,
                    state,
                    *creature,
                    InteractionRoleCode::EncodeCreature,
                )
            } else {
                push_value_surface(surfaces, InteractionRoleCode::Encode, "decline");
            }
        }
        GameAction::ChooseLegend { .. } => {}
        GameAction::ChooseBattleProtector { protector } => {
            push_player_surface(surfaces, *protector, InteractionRoleCode::Protector)
        }
        GameAction::SetAutoPass { .. }
        | GameAction::SetPhaseStops { .. }
        | GameAction::SetPriorityPassingMode { .. }
        | GameAction::SetPriorityYield { .. }
        | GameAction::SetMayTriggerAutoChoice { .. }
        | GameAction::SetTriggerOrderTemplate { .. } => {}
        GameAction::AssignCombatDamage {
            assignments,
            trample_damage,
            controller_damage,
            mode,
        } => {
            for (index, (target, amount)) in assignments.iter().enumerate() {
                push_object_surface(
                    surfaces,
                    state,
                    *target,
                    SurfaceRole::indexed(InteractionRoleCode::DamageTarget, index),
                );
                push_value_surface(
                    surfaces,
                    SurfaceRole::indexed(InteractionRoleCode::DamageAmount, index),
                    amount,
                );
            }
            push_value_surface(
                surfaces,
                InteractionRoleCode::AssignmentMode,
                combat_damage_assignment_mode_code(*mode),
            );
            push_value_surface(surfaces, InteractionRoleCode::TrampleDamage, trample_damage);
            push_value_surface(
                surfaces,
                InteractionRoleCode::ControllerDamage,
                controller_damage,
            );
        }
        GameAction::AssignBlockerDamage { assignments } => {
            for (index, (target, amount)) in assignments.iter().enumerate() {
                push_object_surface(
                    surfaces,
                    state,
                    *target,
                    SurfaceRole::indexed(InteractionRoleCode::DamageTarget, index),
                );
                push_value_surface(
                    surfaces,
                    SurfaceRole::indexed(InteractionRoleCode::DamageAmount, index),
                    amount,
                );
            }
        }
        GameAction::DistributeAmong { distribution } => {
            for (index, (target, amount)) in distribution.iter().enumerate() {
                push_target_surface(
                    surfaces,
                    state,
                    target,
                    SurfaceRole::indexed(InteractionRoleCode::Target, index),
                );
                push_value_surface(
                    surfaces,
                    SurfaceRole::indexed(InteractionRoleCode::Amount, index),
                    amount,
                );
            }
        }
        GameAction::ChooseCounterMoveDistribution { selections } => {
            for (index, selection) in selections.iter().enumerate() {
                push_object_surface(
                    surfaces,
                    state,
                    selection.destination_id,
                    SurfaceRole::indexed(InteractionRoleCode::Destination, index),
                );
                push_value_surface(
                    surfaces,
                    SurfaceRole::indexed(InteractionRoleCode::CounterType, index),
                    selection.counter_type.as_str(),
                );
                push_value_surface(
                    surfaces,
                    SurfaceRole::indexed(InteractionRoleCode::Amount, index),
                    selection.count,
                );
            }
        }
        GameAction::ChooseCountersToRemove { selections } => {
            for (index, selection) in selections.iter().enumerate() {
                push_value_surface(
                    surfaces,
                    SurfaceRole::indexed(InteractionRoleCode::CounterType, index),
                    selection.counter_type.as_str(),
                );
                push_value_surface(
                    surfaces,
                    SurfaceRole::indexed(InteractionRoleCode::Amount, index),
                    selection.count,
                );
            }
        }
        GameAction::SubmitPayAmount { amount } => {
            push_value_surface(surfaces, InteractionRoleCode::Amount, amount)
        }
        GameAction::RetargetSpell { new_targets } => {
            for (index, target) in new_targets.iter().enumerate() {
                push_target_surface(
                    surfaces,
                    state,
                    target,
                    SurfaceRole::indexed(InteractionRoleCode::Target, index),
                );
            }
        }
        GameAction::LearnDecision { choice } => match choice {
            crate::types::actions::LearnOption::Rummage { card_id } => {
                push_object_surface(surfaces, state, *card_id, InteractionRoleCode::DiscardCard)
            }
            crate::types::actions::LearnOption::Skip => {
                push_value_surface(surfaces, InteractionRoleCode::Learn, "skip")
            }
        },
        GameAction::SelectCategoryPermanents { choices } => {
            for (index, choice) in choices.iter().enumerate() {
                if let Some(choice) = choice {
                    push_object_surface(
                        surfaces,
                        state,
                        *choice,
                        SurfaceRole::indexed(InteractionRoleCode::Category, index),
                    );
                } else {
                    push_value_surface(
                        surfaces,
                        SurfaceRole::indexed(InteractionRoleCode::Category, index),
                        "none",
                    );
                }
            }
        }
        GameAction::ChooseKeptCreatures { kept } | GameAction::ChooseKeptPermanents { kept } => {
            push_object_list(surfaces, state, kept, InteractionRoleCode::Kept)
        }
        GameAction::ChooseX { value } => {
            push_value_surface(surfaces, InteractionRoleCode::X, value)
        }
        GameAction::SubmitPhyrexianChoices { choices } => {
            for choice in choices {
                push_value_surface(
                    surfaces,
                    InteractionRoleCode::PhyrexianPayment,
                    shard_choice_code(*choice),
                );
            }
        }
        GameAction::ChooseManaColor { choice, count } => {
            let symbols = match choice {
                crate::types::game_state::ManaChoice::SingleColor(mana_type) => {
                    vec![mana_type_code(*mana_type).to_string()]
                }
                crate::types::game_state::ManaChoice::Combination(mana_types) => mana_types
                    .iter()
                    .map(|mana_type| mana_type_code(*mana_type).to_string())
                    .collect(),
            };
            surfaces.push(InteractionPresentationSurface::Mana {
                role: InteractionRoleCode::ManaChoice,
                index: None,
                symbols,
            });
            push_value_surface(surfaces, InteractionRoleCode::Count, count);
        }
        GameAction::PayManaAbilityMana { payment } => {
            surfaces.push(InteractionPresentationSurface::Mana {
                role: InteractionRoleCode::ManaPayment,
                index: None,
                symbols: payment
                    .iter()
                    .map(|mana_type| mana_type_code(*mana_type).to_string())
                    .collect(),
            });
        }
        GameAction::ChooseSpecializeColor { color } => push_value_surface(
            surfaces,
            InteractionRoleCode::Color,
            mana_color_code(*color),
        ),
        GameAction::Debug(_) => {}
        GameAction::GrantDebugPermission { player_id }
        | GameAction::RevokeDebugPermission { player_id }
        | GameAction::Concede { player_id } => {
            push_player_surface(surfaces, *player_id, InteractionRoleCode::Player)
        }
        GameAction::DeclareShortcut { .. } => {
            surfaces.push(InteractionPresentationSurface::ShortcutResponse {
                response: InteractionShortcutResponseCode::Propose,
            });
        }
        GameAction::DeclineShortcut => {
            surfaces.push(InteractionPresentationSurface::ShortcutResponse {
                response: InteractionShortcutResponseCode::Decline,
            });
        }
        GameAction::RespondToShortcut { response } => {
            let response = match response {
                crate::analysis::loop_check::ShortcutResponse::Accept => {
                    InteractionShortcutResponseCode::Accept
                }
                crate::analysis::loop_check::ShortcutResponse::Shorten { .. } => {
                    InteractionShortcutResponseCode::Shorten
                }
            };
            surfaces.push(InteractionPresentationSurface::ShortcutResponse { response });
        }
        GameAction::PrecastCopyShortcut { response, .. } => {
            let response = match response {
                PrecastCopyShortcutResponse::Propose { .. } => {
                    InteractionShortcutResponseCode::Propose
                }
                PrecastCopyShortcutResponse::Decline => InteractionShortcutResponseCode::Decline,
                PrecastCopyShortcutResponse::Accept => InteractionShortcutResponseCode::Accept,
                PrecastCopyShortcutResponse::Shorten { .. } => {
                    InteractionShortcutResponseCode::Shorten
                }
            };
            surfaces.push(InteractionPresentationSurface::ShortcutResponse { response });
        }
    }
}

fn project_prompt_payload(
    action: &GameAction,
    state: &GameState,
    surfaces: &mut Vec<InteractionPresentationSurface>,
) {
    match (&state.waiting_for, action) {
        (WaitingFor::ModeChoice { modal, .. }, GameAction::SelectModes { indices })
        | (WaitingFor::AbilityModeChoice { modal, .. }, GameAction::SelectModes { indices }) => {
            for index in indices {
                if let Some(description) = modal.mode_descriptions.get(*index) {
                    push_value_surface(surfaces, InteractionRoleCode::Mode, description);
                }
                if let Some(cost) = modal.mode_costs.get(*index) {
                    surfaces.push(InteractionPresentationSurface::Mana {
                        role: InteractionRoleCode::ModeCost,
                        index: None,
                        symbols: mana_cost_symbols(cost),
                    });
                }
            }
        }
        (
            WaitingFor::CastingVariantChoice { options, .. },
            GameAction::ChooseCastingVariant { index },
        ) => {
            if let Some(option) = options.get(*index) {
                project_casting_variant(option.variant, state, surfaces);
                surfaces.push(InteractionPresentationSurface::Mana {
                    role: InteractionRoleCode::CastingCost,
                    index: None,
                    symbols: mana_cost_symbols(&option.mana_cost),
                });
            }
        }
        (
            WaitingFor::VoteChoice {
                option_labels,
                candidate_objects,
                ..
            },
            GameAction::SubmitVoteCandidate { candidate_index },
        ) => {
            let index = *candidate_index as usize;
            if let Some(label) = option_labels.get(index) {
                push_value_surface(surfaces, InteractionRoleCode::VoteOption, label);
            }
            if let Some(object_id) = candidate_objects.get(index) {
                push_object_surface(
                    surfaces,
                    state,
                    *object_id,
                    InteractionRoleCode::VoteCandidate,
                );
            }
        }
        _ => {}
    }
}

fn action_code(action: &GameAction) -> InteractionActionCode {
    match action {
        GameAction::PassPriority => InteractionActionCode::PassPriority,
        GameAction::ChooseMeldPair { .. } => InteractionActionCode::ChooseMeldPair,
        GameAction::ChooseEntryAttackTarget { .. } => {
            InteractionActionCode::ChooseEntryAttackTarget
        }
        GameAction::PlayLand { .. } => InteractionActionCode::PlayLand,
        GameAction::CastSpell { .. } => InteractionActionCode::CastSpell,
        GameAction::Foretell { .. } => InteractionActionCode::Foretell,
        GameAction::ActivateAbility { .. } => InteractionActionCode::ActivateAbility,
        GameAction::DeclareAttackers { .. } => InteractionActionCode::DeclareAttackers,
        GameAction::DeclareBlockers { .. } => InteractionActionCode::DeclareBlockers,
        GameAction::ChooseUntap { .. } => InteractionActionCode::ChooseUntap,
        GameAction::ChooseExert { .. } => InteractionActionCode::ChooseExert,
        GameAction::ChooseEnlist { .. } => InteractionActionCode::ChooseEnlist,
        GameAction::ChooseClashOpponent { .. } => InteractionActionCode::ChooseClashOpponent,
        GameAction::ChooseZoneOpponentChooser { .. } => {
            InteractionActionCode::ChooseZoneOpponentChooser
        }
        GameAction::ChoosePileOpponent { .. } => InteractionActionCode::ChoosePileOpponent,
        GameAction::ChooseAnnouncingOpponent { .. } => {
            InteractionActionCode::ChooseAnnouncingOpponent
        }
        GameAction::ChooseAssistPlayer { .. } => InteractionActionCode::ChooseAssistPlayer,
        GameAction::CommitAssistPayment { .. } => InteractionActionCode::CommitAssistPayment,
        GameAction::MulliganDecision { .. } => InteractionActionCode::MulliganDecision,
        GameAction::ReorderHand { .. } => InteractionActionCode::ReorderHand,
        GameAction::TapLandForMana { .. } => InteractionActionCode::TapLandForMana,
        GameAction::UntapLandForMana { .. } => InteractionActionCode::UntapLandForMana,
        GameAction::SpendPoolMana { .. } => InteractionActionCode::SpendPoolMana,
        GameAction::UnspendPoolMana { .. } => InteractionActionCode::UnspendPoolMana,
        GameAction::SelectCards { .. } => InteractionActionCode::SelectCards,
        GameAction::ChooseRemoveCounterCostDistribution { .. } => {
            InteractionActionCode::ChooseRemoveCounterCostDistribution
        }
        GameAction::SelectCoinFlips { .. } => InteractionActionCode::SelectCoinFlips,
        GameAction::ChooseOutsideGameCards { .. } => InteractionActionCode::ChooseOutsideGameCards,
        GameAction::SelectTargets { .. } => InteractionActionCode::SelectTargets,
        GameAction::ChooseTarget { .. } => InteractionActionCode::ChooseTarget,
        GameAction::ChooseReplacement { .. } => InteractionActionCode::ChooseReplacement,
        GameAction::OrderTriggers { .. } => InteractionActionCode::OrderTriggers,
        GameAction::CancelCast => InteractionActionCode::CancelCast,
        GameAction::Equip { .. } => InteractionActionCode::Equip,
        GameAction::CrewVehicle { .. } => InteractionActionCode::CrewVehicle,
        GameAction::ActivateStation { .. } => InteractionActionCode::ActivateStation,
        GameAction::SaddleMount { .. } => InteractionActionCode::SaddleMount,
        GameAction::Transform { .. } => InteractionActionCode::Transform,
        GameAction::PlayFaceDown { .. } => InteractionActionCode::PlayFaceDown,
        GameAction::TurnFaceUp { .. } => InteractionActionCode::TurnFaceUp,
        GameAction::SubmitSideboard { .. } => InteractionActionCode::SubmitSideboard,
        GameAction::ChoosePlayDraw { .. } => InteractionActionCode::ChoosePlayDraw,
        GameAction::ChooseOption { .. } => InteractionActionCode::ChooseOption,
        GameAction::SubmitVoteCandidate { .. } => InteractionActionCode::SubmitVoteCandidate,
        GameAction::SubmitSpellbookDraft { .. } => InteractionActionCode::SubmitSpellbookDraft,
        GameAction::SubmitPilePartition { .. } => InteractionActionCode::SubmitPilePartition,
        GameAction::ChoosePile { .. } => InteractionActionCode::ChoosePile,
        GameAction::ChooseBranch { .. } => InteractionActionCode::ChooseBranch,
        GameAction::SubmitLifeRedistribution { .. } => {
            InteractionActionCode::SubmitLifeRedistribution
        }
        GameAction::ChooseDamageSource { .. } => InteractionActionCode::ChooseDamageSource,
        GameAction::SelectModes { .. } => InteractionActionCode::SelectModes,
        GameAction::DecideOptionalCost { .. } => InteractionActionCode::DecideOptionalCost,
        GameAction::ChooseAdventureFace { .. } => InteractionActionCode::ChooseAdventureFace,
        GameAction::ChooseModalFace { .. } => InteractionActionCode::ChooseModalFace,
        GameAction::ChooseAlternativeCast { .. } => InteractionActionCode::ChooseAlternativeCast,
        GameAction::ChooseCastingVariant { .. } => InteractionActionCode::ChooseCastingVariant,
        GameAction::KeepAllCopyTargets => InteractionActionCode::KeepAllCopyTargets,
        GameAction::ChoosePermanentTypeSlot { .. } => {
            InteractionActionCode::ChoosePermanentTypeSlot
        }
        GameAction::ActivateNinjutsu { .. } => InteractionActionCode::ActivateNinjutsu,
        GameAction::CastSpellAsSneak { .. } => InteractionActionCode::CastSpellAsSneak,
        GameAction::CastSpellAsWebSlinging { .. } => InteractionActionCode::CastSpellAsWebSlinging,
        GameAction::CastSpellForFree { .. } => InteractionActionCode::CastSpellForFree,
        GameAction::CastSpellAsMiracle { .. } => InteractionActionCode::CastSpellAsMiracle,
        GameAction::CastSpellAsMadness { .. } => InteractionActionCode::CastSpellAsMadness,
        GameAction::DecideOptionalEffect { .. } => InteractionActionCode::DecideOptionalEffect,
        GameAction::RespondToSpliceOffer { .. } => InteractionActionCode::RespondToSpliceOffer,
        GameAction::DecideOptionalEffectAndRemember { .. } => {
            InteractionActionCode::DecideOptionalEffectAndRemember
        }
        GameAction::PayUnlessCost { .. } => InteractionActionCode::PayUnlessCost,
        GameAction::ChooseUnlessCostBranch { .. } => InteractionActionCode::ChooseUnlessCostBranch,
        GameAction::ChooseActivationCostBranch { .. } => {
            InteractionActionCode::ChooseActivationCostBranch
        }
        GameAction::PayCombatTax { .. } => InteractionActionCode::PayCombatTax,
        GameAction::ChooseRingBearer { .. } => InteractionActionCode::ChooseRingBearer,
        GameAction::ChoosePair { .. } => InteractionActionCode::ChoosePair,
        GameAction::ChooseDungeon { .. } => InteractionActionCode::ChooseDungeon,
        GameAction::ChooseDungeonRoom { .. } => InteractionActionCode::ChooseDungeonRoom,
        GameAction::UnlockRoomDoor { .. } => InteractionActionCode::UnlockRoomDoor,
        GameAction::RollPlanarDie => InteractionActionCode::RollPlanarDie,
        GameAction::ChooseRoomDoor { .. } => InteractionActionCode::ChooseRoomDoor,
        GameAction::TapForConvoke { .. } => InteractionActionCode::TapForConvoke,
        GameAction::HarmonizeTap { .. } => InteractionActionCode::HarmonizeTap,
        GameAction::DeclareCompanion { .. } => InteractionActionCode::DeclareCompanion,
        GameAction::CompanionToHand => InteractionActionCode::CompanionToHand,
        GameAction::DiscoverChoice { .. } => InteractionActionCode::DiscoverChoice,
        GameAction::GraveyardPaidCastChoice { .. } => {
            InteractionActionCode::GraveyardPaidCastChoice
        }
        GameAction::CascadeChoice { .. } => InteractionActionCode::CascadeChoice,
        GameAction::RippleChoice { .. } => InteractionActionCode::RippleChoice,
        GameAction::FreeCastWindowChoice { .. } => InteractionActionCode::FreeCastWindowChoice,
        GameAction::ChooseTopOrBottom { .. } => InteractionActionCode::ChooseTopOrBottom,
        GameAction::ChooseMutateMergeSide { .. } => InteractionActionCode::ChooseMutateMergeSide,
        GameAction::CipherEncode { .. } => InteractionActionCode::CipherEncode,
        GameAction::ChooseLegend { .. } => InteractionActionCode::ChooseLegend,
        GameAction::ChooseBattleProtector { .. } => InteractionActionCode::ChooseBattleProtector,
        GameAction::SetAutoPass { .. } => InteractionActionCode::SetAutoPass,
        GameAction::CancelAutoPass => InteractionActionCode::CancelAutoPass,
        GameAction::SetPhaseStops { .. } => InteractionActionCode::SetPhaseStops,
        GameAction::SetPriorityPassingMode { .. } => InteractionActionCode::SetPriorityPassingMode,
        GameAction::SetPriorityYield { .. } => InteractionActionCode::SetPriorityYield,
        GameAction::SetMayTriggerAutoChoice { .. } => {
            InteractionActionCode::SetMayTriggerAutoChoice
        }
        GameAction::SetTriggerOrderTemplate { .. } => {
            InteractionActionCode::SetTriggerOrderTemplate
        }
        GameAction::AssignCombatDamage { .. } => InteractionActionCode::AssignCombatDamage,
        GameAction::AssignBlockerDamage { .. } => InteractionActionCode::AssignBlockerDamage,
        GameAction::DistributeAmong { .. } => InteractionActionCode::DistributeAmong,
        GameAction::ChooseCounterMoveDistribution { .. } => {
            InteractionActionCode::ChooseCounterMoveDistribution
        }
        GameAction::ChooseCountersToRemove { .. } => InteractionActionCode::ChooseCountersToRemove,
        GameAction::SubmitPayAmount { .. } => InteractionActionCode::SubmitPayAmount,
        GameAction::RetargetSpell { .. } => InteractionActionCode::RetargetSpell,
        GameAction::LearnDecision { .. } => InteractionActionCode::LearnDecision,
        GameAction::SelectCategoryPermanents { .. } => {
            InteractionActionCode::SelectCategoryPermanents
        }
        GameAction::ChooseKeptCreatures { .. } => InteractionActionCode::ChooseKeptCreatures,
        GameAction::ChooseKeptPermanents { .. } => InteractionActionCode::ChooseKeptPermanents,
        GameAction::ChooseX { .. } => InteractionActionCode::ChooseX,
        GameAction::SubmitPhyrexianChoices { .. } => InteractionActionCode::SubmitPhyrexianChoices,
        GameAction::ChooseManaColor { .. } => InteractionActionCode::ChooseManaColor,
        GameAction::PayManaAbilityMana { .. } => InteractionActionCode::PayManaAbilityMana,
        GameAction::CastPreparedCopy { .. } => InteractionActionCode::CastPreparedCopy,
        GameAction::ChooseSpecializeColor { .. } => InteractionActionCode::ChooseSpecializeColor,
        GameAction::CastParadigmCopy { .. } => InteractionActionCode::CastParadigmCopy,
        GameAction::PassParadigmOffer => InteractionActionCode::PassParadigmOffer,
        GameAction::GrantDebugPermission { .. } => InteractionActionCode::GrantDebugPermission,
        GameAction::RevokeDebugPermission { .. } => InteractionActionCode::RevokeDebugPermission,
        GameAction::Concede { .. } => InteractionActionCode::Concede,
        GameAction::DeclareShortcut { .. } => InteractionActionCode::DeclareShortcut,
        GameAction::RespondToShortcut { .. } => InteractionActionCode::RespondToShortcut,
        GameAction::DeclineShortcut => InteractionActionCode::DeclineShortcut,
        GameAction::PrecastCopyShortcut { .. } => InteractionActionCode::PrecastCopyShortcut,
        GameAction::Debug(_) => InteractionActionCode::Debug,
    }
}

fn action_surfaces(
    action: &GameAction,
    filtered_state: &GameState,
) -> Vec<InteractionPresentationSurface> {
    let mut surfaces = vec![
        InteractionPresentationSurface::Summary {
            code: InteractionSummaryCode::Candidate,
        },
        InteractionPresentationSurface::Action {
            code: action_code(action),
        },
    ];
    if let Some(source) = action.source_object() {
        push_object_surface(
            &mut surfaces,
            filtered_state,
            source,
            InteractionRoleCode::Source,
        );
    }
    project_action_payload(action, filtered_state, &mut surfaces);
    project_prompt_payload(action, filtered_state, &mut surfaces);
    surfaces
}

fn actor_candidates(
    state: &GameState,
    semantic_owner: PlayerId,
) -> Result<Vec<CandidateAction>, InteractionReasonCode> {
    let mut candidates = validated_candidate_actions_for_semantic_owner(state, semantic_owner);
    if candidates.len() > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    let manual_cast_candidates = candidates
        .iter()
        .filter_map(|candidate| {
            let mut manual = candidate.clone();
            let payment_mode = manual.action.payment_mode_mut()?;
            if *payment_mode != CastPaymentMode::Auto {
                return None;
            }
            *payment_mode = CastPaymentMode::Manual;
            Some(manual)
        })
        .collect::<Vec<_>>();
    let pipeline = FilterPipeline::default_pipeline();
    for candidate in manual_cast_candidates {
        // CR 601.2g: manual payment is a distinct human declaration path. Validate each
        // sibling independently through the canonical reducer-backed legality pipeline;
        // never infer its legality merely from the corresponding Auto candidate.
        if !pipeline.accepts(state, &candidate) {
            continue;
        }
        if candidates.len() == MAX_INTERACTION_LIST_LEN {
            return Err(InteractionReasonCode::PayloadTooLarge);
        }
        candidates.push(candidate);
    }
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        let mana_actions =
            super::mana_sources::activatable_mana_actions_for_player(state, semantic_owner);
        let undo_actions = state
            .lands_tapped_for_mana
            .get(&semantic_owner)
            .into_iter()
            .flatten()
            .copied()
            .map(|object_id| GameAction::UntapLandForMana { object_id });
        for action in mana_actions.into_iter().chain(undo_actions) {
            if candidates
                .iter()
                .any(|candidate| candidate.action == action)
            {
                continue;
            }
            if candidates.len() == MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            candidates.push(CandidateAction {
                action,
                metadata: ActionMetadata::for_actor(Some(semantic_owner), TacticalClass::Mana),
            });
        }
    }
    candidates.sort_by(|a, b| a.action.cmp_stable(&b.action));
    Ok(candidates)
}

fn is_escape_action(action: &GameAction) -> bool {
    matches!(action, GameAction::CancelCast)
}

fn exact_choices(
    interaction_id: &InteractionId,
    candidates: &[CandidateAction],
    filtered_state: &GameState,
) -> Vec<InteractionChoice> {
    candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| InteractionChoice {
            id: interaction_choice_id(interaction_id, 'c', index),
            surfaces: action_surfaces(&candidate.action, filtered_state),
            status: InteractionChoiceStatus::Available,
        })
        .collect()
}

fn direct_choices(
    interaction_id: &InteractionId,
    projection: &DirectChoiceProjection,
    filtered_state: &GameState,
) -> Vec<InteractionChoice> {
    projection
        .actions
        .iter()
        .enumerate()
        .map(|(index, action)| InteractionChoice {
            id: interaction_choice_id(interaction_id, 'p', index),
            surfaces: action_surfaces(action, filtered_state),
            status: InteractionChoiceStatus::Available,
        })
        .collect()
}

fn loop_shortcut_choices(
    interaction_id: &InteractionId,
    projection: &LoopShortcutProjection,
    filtered_state: &GameState,
) -> Vec<InteractionChoice> {
    projection
        .points
        .iter()
        .flat_map(|point| {
            point.candidate_indices.iter().map(move |index| {
                let mut surfaces = vec![InteractionPresentationSurface::Summary {
                    code: InteractionSummaryCode::Candidate,
                }];
                match &projection.candidates[*index] {
                    LoopShortcutCandidateValue::Target(target) => push_target_surface(
                        &mut surfaces,
                        filtered_state,
                        target,
                        InteractionRoleCode::Target.into(),
                    ),
                    LoopShortcutCandidateValue::ConvokeObject(object_id) => push_object_surface(
                        &mut surfaces,
                        filtered_state,
                        *object_id,
                        InteractionRoleCode::ConvokeMana,
                    ),
                    LoopShortcutCandidateValue::Mode(mode) => surfaces.push(
                        InteractionPresentationSurface::Value {
                            role: InteractionRoleCode::ModeIndex,
                            index: Some(
                                u32::try_from(*mode)
                                    .expect("loop shortcut projection bounded mode indices"),
                            ),
                            value: mode.to_string(),
                        },
                    ),
                    LoopShortcutCandidateValue::May(choice) => surfaces.push(
                        InteractionPresentationSurface::Value {
                            role: InteractionRoleCode::Accept,
                            index: None,
                            value: match choice {
                                crate::analysis::decision_template::MayChoiceOption::Take => "take",
                                crate::analysis::decision_template::MayChoiceOption::Decline => {
                                    "decline"
                                }
                            }
                            .to_string(),
                        },
                    ),
                    LoopShortcutCandidateValue::Unless(choice) => surfaces.push(
                        InteractionPresentationSurface::Value {
                            role: InteractionRoleCode::Pay,
                            index: None,
                            value: match choice {
                                crate::analysis::decision_template::UnlessPaymentOption::Pay => {
                                    "pay"
                                }
                                crate::analysis::decision_template::UnlessPaymentOption::Decline => {
                                    "decline"
                                }
                            }
                            .to_string(),
                        },
                    ),
                    LoopShortcutCandidateValue::ManaColor(color) => surfaces.push(
                        InteractionPresentationSurface::Mana {
                            role: InteractionRoleCode::Color,
                            index: None,
                            symbols: vec![mana_color_symbol(*color).to_string()],
                        },
                    ),
                }
                InteractionChoice {
                    id: interaction_choice_id(interaction_id, 'k', *index),
                    surfaces,
                    status: InteractionChoiceStatus::Available,
                }
            })
        })
        .collect()
}

fn loop_shortcut_points(
    interaction_id: &InteractionId,
    projection: &LoopShortcutProjection,
) -> Vec<InteractionShortcutPoint> {
    projection
        .points
        .iter()
        .enumerate()
        .map(|(group, point)| InteractionShortcutPoint {
            group: group as u32,
            kind: point.kind,
            min: point.min,
            max: point.max,
            unique: point.unique,
            ordered: point.ordered,
            read_only: point.read_only,
            candidate_ids: point
                .candidate_indices
                .iter()
                .map(|index| interaction_choice_id(interaction_id, 'k', *index))
                .collect(),
        })
        .collect()
}

fn sideboard_choices(
    interaction_id: &InteractionId,
    projection: &SideboardProjection,
) -> Vec<InteractionChoice> {
    projection
        .cards
        .iter()
        .enumerate()
        .map(|(index, card)| InteractionChoice {
            id: interaction_choice_id(interaction_id, 'b', index),
            surfaces: vec![
                InteractionPresentationSurface::Summary {
                    code: InteractionSummaryCode::Candidate,
                },
                InteractionPresentationSurface::Value {
                    role: InteractionRoleCode::CardName,
                    index: Some(index as u32),
                    value: card.name.clone(),
                },
                InteractionPresentationSurface::Amount {
                    min: 0,
                    max: card.total,
                    total: Some(card.current_main),
                },
            ],
            status: InteractionChoiceStatus::Available,
        })
        .collect()
}

fn selection_choices(
    interaction_id: &InteractionId,
    selection: &SelectionProjection,
    filtered_state: &GameState,
) -> Vec<InteractionChoice> {
    selection
        .object_ids
        .iter()
        .enumerate()
        .map(|(index, object_id)| InteractionChoice {
            id: interaction_choice_id(interaction_id, 's', index),
            surfaces: std::iter::once(InteractionPresentationSurface::Summary {
                code: InteractionSummaryCode::Candidate,
            })
            .chain(object_surface(
                filtered_state,
                *object_id,
                InteractionRoleCode::Candidate,
            ))
            .collect(),
            status: InteractionChoiceStatus::Available,
        })
        .collect()
}

fn counter_assignment_choices(
    interaction_id: &InteractionId,
    projection: &CounterDistributionProjection,
    filtered_state: &GameState,
) -> Vec<InteractionChoice> {
    projection
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| InteractionChoice {
            id: interaction_choice_id(interaction_id, 'a', index),
            surfaces: std::iter::once(InteractionPresentationSurface::Summary {
                code: InteractionSummaryCode::Candidate,
            })
            .chain(object_surface(
                filtered_state,
                candidate.object_id,
                InteractionRoleCode::Candidate,
            ))
            .chain([
                InteractionPresentationSurface::Counter {
                    counter_type: candidate.counter_type.as_str().into_owned(),
                    available: candidate.available,
                },
                InteractionPresentationSurface::Amount {
                    min: 0,
                    max: candidate.available,
                    total: Some(projection.total),
                },
            ])
            .collect(),
            status: InteractionChoiceStatus::Available,
        })
        .collect()
}

fn amount_assignment_choices(
    interaction_id: &InteractionId,
    candidates: &[AssignmentCandidate],
    total: Option<u32>,
    filtered_state: &GameState,
) -> Vec<InteractionChoice> {
    candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let mut surfaces = vec![InteractionPresentationSurface::Summary {
                code: InteractionSummaryCode::Candidate,
            }];
            match &candidate.kind {
                AssignmentCandidateKind::Object(object_id) => push_object_surface(
                    &mut surfaces,
                    filtered_state,
                    *object_id,
                    InteractionRoleCode::DamageTarget,
                ),
                AssignmentCandidateKind::Target(target) => push_target_surface(
                    &mut surfaces,
                    filtered_state,
                    target,
                    InteractionRoleCode::DamageTarget.into(),
                ),
                AssignmentCandidateKind::CounterMove {
                    destination_id,
                    counter_type,
                } => {
                    push_object_surface(
                        &mut surfaces,
                        filtered_state,
                        *destination_id,
                        InteractionRoleCode::Destination,
                    );
                    surfaces.push(InteractionPresentationSurface::Counter {
                        counter_type: counter_type.as_str().into_owned(),
                        available: candidate.available,
                    });
                }
                AssignmentCandidateKind::CounterRemove { counter_type } => {
                    surfaces.push(InteractionPresentationSurface::Counter {
                        counter_type: counter_type.as_str().into_owned(),
                        available: candidate.available,
                    });
                }
            }
            surfaces.push(InteractionPresentationSurface::Amount {
                min: 0,
                max: candidate.available,
                total,
            });
            InteractionChoice {
                id: interaction_choice_id(interaction_id, 'a', index),
                surfaces,
                status: InteractionChoiceStatus::Available,
            }
        })
        .collect()
}

fn trigger_order_choices(
    interaction_id: &InteractionId,
    projection: &TriggerOrderProjection,
    filtered_state: &GameState,
) -> Vec<InteractionChoice> {
    let WaitingFor::OrderTriggers { triggers, .. } = &filtered_state.waiting_for else {
        unreachable!("trigger-order projection requires an OrderTriggers prompt");
    };
    debug_assert_eq!(projection.count, triggers.len());
    triggers
        .iter()
        .enumerate()
        .map(|(index, trigger)| InteractionChoice {
            id: interaction_choice_id(interaction_id, 'q', index),
            surfaces: std::iter::once(InteractionPresentationSurface::Summary {
                code: InteractionSummaryCode::Candidate,
            })
            .chain(object_surface(
                filtered_state,
                trigger.source_id,
                SurfaceRole::indexed(InteractionRoleCode::TriggerIndex, index),
            ))
            .chain(std::iter::once(InteractionPresentationSurface::Value {
                role: InteractionRoleCode::TriggerIndex,
                index: Some(index.min(u32::MAX as usize) as u32),
                value: index.to_string(),
            }))
            .collect(),
            status: InteractionChoiceStatus::Available,
        })
        .collect()
}

fn coin_flip_choices(
    interaction_id: &InteractionId,
    projection: CoinFlipProjection,
) -> Vec<InteractionChoice> {
    (0..projection.candidate_count)
        .map(|index| InteractionChoice {
            id: interaction_choice_id(interaction_id, 'f', index),
            surfaces: vec![
                InteractionPresentationSurface::Summary {
                    code: InteractionSummaryCode::Candidate,
                },
                InteractionPresentationSurface::Value {
                    role: InteractionRoleCode::CoinFlipIndex,
                    index: Some(index as u32),
                    value: index.to_string(),
                },
            ],
            status: InteractionChoiceStatus::Available,
        })
        .collect()
}

fn target_sequence_choices(
    interaction_id: &InteractionId,
    projection: &TargetSequenceProjection,
    filtered_state: &GameState,
) -> Vec<InteractionChoice> {
    projection
        .candidates
        .iter()
        .enumerate()
        .map(|(index, target)| {
            let mut surfaces = vec![InteractionPresentationSurface::Summary {
                code: InteractionSummaryCode::Candidate,
            }];
            push_target_surface(
                &mut surfaces,
                filtered_state,
                target,
                InteractionRoleCode::Candidate.into(),
            );
            InteractionChoice {
                id: interaction_choice_id(interaction_id, 't', index),
                surfaces,
                status: InteractionChoiceStatus::Available,
            }
        })
        .collect()
}

fn category_selection_choices(
    interaction_id: &InteractionId,
    projection: &CategorySelectionProjection,
    filtered_state: &GameState,
) -> Vec<InteractionChoice> {
    projection
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| InteractionChoice {
            id: interaction_choice_id(interaction_id, 'g', index),
            surfaces: std::iter::once(InteractionPresentationSurface::Summary {
                code: InteractionSummaryCode::Candidate,
            })
            .chain(object_surface(
                filtered_state,
                candidate.object_id,
                InteractionRoleCode::Candidate,
            ))
            .chain(std::iter::once(InteractionPresentationSurface::Value {
                role: InteractionRoleCode::Category,
                index: Some(candidate.group as u32),
                value: candidate.category.to_string(),
            }))
            .collect(),
            status: InteractionChoiceStatus::Available,
        })
        .collect()
}

fn combat_relation_choices(
    interaction_id: &InteractionId,
    projection: &CombatRelationProjection,
    filtered_state: &GameState,
) -> Vec<InteractionChoice> {
    let source_role = match projection.action {
        CombatRelationAction::Attackers => InteractionRoleCode::Attacker,
        CombatRelationAction::Blockers => InteractionRoleCode::Blocker,
    };
    let sources = projection
        .sources
        .iter()
        .enumerate()
        .map(|(index, object_id)| InteractionChoice {
            id: interaction_choice_id(interaction_id, 'r', index),
            surfaces: std::iter::once(InteractionPresentationSurface::Summary {
                code: InteractionSummaryCode::Candidate,
            })
            .chain(object_surface(filtered_state, *object_id, source_role))
            .collect(),
            status: InteractionChoiceStatus::Available,
        });
    let targets = projection
        .targets
        .iter()
        .enumerate()
        .map(|(index, target)| {
            let mut surfaces = vec![InteractionPresentationSurface::Summary {
                code: InteractionSummaryCode::Candidate,
            }];
            match target {
                CombatRelationTarget::Attack(target) => push_attack_target_surface(
                    &mut surfaces,
                    filtered_state,
                    target,
                    InteractionRoleCode::AttackTarget,
                ),
                CombatRelationTarget::Object(object_id) => push_object_surface(
                    &mut surfaces,
                    filtered_state,
                    *object_id,
                    InteractionRoleCode::Blocked,
                ),
            }
            InteractionChoice {
                id: interaction_choice_id(interaction_id, 'd', index),
                surfaces,
                status: InteractionChoiceStatus::Available,
            }
        });
    sources.chain(targets).collect()
}

fn combat_relation_constraints(
    interaction_id: &InteractionId,
    projection: &CombatRelationProjection,
) -> Vec<InteractionRelationConstraint> {
    projection
        .legal_target_indices
        .iter()
        .enumerate()
        .map(
            |(source_index, target_indices)| InteractionRelationConstraint {
                source_id: interaction_choice_id(interaction_id, 'r', source_index),
                target_ids: target_indices
                    .iter()
                    .map(|target_index| interaction_choice_id(interaction_id, 'd', *target_index))
                    .collect(),
            },
        )
        .collect()
}

fn mana_group_choices(
    interaction_id: &InteractionId,
    projection: &ManaGroupProjection,
) -> Vec<InteractionChoice> {
    let mut choices = projection
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let mut surfaces = vec![InteractionPresentationSurface::Summary {
                code: InteractionSummaryCode::Candidate,
            }];
            match candidate.value {
                ManaGroupCandidateValue::Mana(mana_type) => {
                    surfaces.push(InteractionPresentationSurface::Mana {
                        role: InteractionRoleCode::ManaChoice,
                        index: Some(candidate.group as u32),
                        symbols: vec![mana_type_code(mana_type).to_string()],
                    });
                }
                ManaGroupCandidateValue::Phyrexian { choice, color } => {
                    surfaces.push(InteractionPresentationSurface::Mana {
                        role: InteractionRoleCode::PhyrexianPayment,
                        index: Some(candidate.group as u32),
                        symbols: vec![mana_color_code(color).to_string()],
                    });
                    push_value_surface(
                        &mut surfaces,
                        SurfaceRole::indexed(
                            InteractionRoleCode::PhyrexianPayment,
                            candidate.group,
                        ),
                        shard_choice_code(choice),
                    );
                }
            }
            InteractionChoice {
                id: interaction_choice_id(interaction_id, 'm', index),
                surfaces,
                status: InteractionChoiceStatus::Available,
            }
        })
        .collect::<Vec<_>>();
    if projection.allow_cancel {
        choices.push(InteractionChoice {
            id: interaction_choice_id(interaction_id, 'e', 0),
            surfaces: vec![
                InteractionPresentationSurface::Summary {
                    code: InteractionSummaryCode::Cancel,
                },
                InteractionPresentationSurface::Action {
                    code: InteractionActionCode::CancelCast,
                },
            ],
            status: InteractionChoiceStatus::Available,
        });
    }
    choices
}

fn mode_sequence_choices(
    interaction_id: &InteractionId,
    projection: &ModeSequenceProjection,
) -> Vec<InteractionChoice> {
    let mut choices = projection
        .indices
        .iter()
        .zip(&projection.descriptions)
        .enumerate()
        .map(|(candidate_index, (mode_index, description))| {
            let mut surfaces = vec![
                InteractionPresentationSurface::Summary {
                    code: InteractionSummaryCode::Candidate,
                },
                InteractionPresentationSurface::Value {
                    role: InteractionRoleCode::ModeIndex,
                    index: Some(*mode_index as u32),
                    value: mode_index.to_string(),
                },
            ];
            if let Some(description) = description {
                surfaces.push(InteractionPresentationSurface::Value {
                    role: InteractionRoleCode::Mode,
                    index: Some(*mode_index as u32),
                    value: description.clone(),
                });
            }
            InteractionChoice {
                id: interaction_choice_id(interaction_id, 'o', candidate_index),
                surfaces,
                status: InteractionChoiceStatus::Available,
            }
        })
        .collect::<Vec<_>>();
    if projection.allow_cancel {
        choices.push(InteractionChoice {
            id: interaction_choice_id(interaction_id, 'e', 0),
            surfaces: vec![
                InteractionPresentationSurface::Summary {
                    code: InteractionSummaryCode::Cancel,
                },
                InteractionPresentationSurface::Action {
                    code: InteractionActionCode::CancelCast,
                },
            ],
            status: InteractionChoiceStatus::Available,
        });
    }
    choices
}

fn outside_selection_choices(
    interaction_id: &InteractionId,
    projection: &OutsideSelectionProjection,
    filtered_state: &GameState,
) -> Vec<InteractionChoice> {
    projection
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let mut surfaces = vec![InteractionPresentationSurface::Summary {
                code: InteractionSummaryCode::Candidate,
            }];
            match candidate.selection {
                OutsideGameSelection::Sideboard { sideboard_index } => {
                    push_value_surface(
                        &mut surfaces,
                        InteractionRoleCode::SideboardIndex,
                        sideboard_index,
                    );
                    push_value_surface(
                        &mut surfaces,
                        InteractionRoleCode::CardName,
                        &candidate.name,
                    );
                }
                OutsideGameSelection::FaceUpExile { object_id } => {
                    push_object_surface(
                        &mut surfaces,
                        filtered_state,
                        object_id,
                        InteractionRoleCode::FaceUpExile,
                    );
                }
            }
            InteractionChoice {
                id: interaction_choice_id(interaction_id, 'w', index),
                surfaces,
                status: InteractionChoiceStatus::Available,
            }
        })
        .collect()
}

fn text_choice_suggestions(
    interaction_id: &InteractionId,
    projection: &TextChoiceProjection,
) -> Vec<InteractionChoice> {
    projection
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| InteractionChoice {
            id: interaction_choice_id(interaction_id, 'n', index),
            surfaces: vec![
                InteractionPresentationSurface::Summary {
                    code: InteractionSummaryCode::Candidate,
                },
                InteractionPresentationSurface::Value {
                    role: InteractionRoleCode::Choice,
                    index: Some(index as u32),
                    value: option.clone(),
                },
            ],
            status: InteractionChoiceStatus::Available,
        })
        .collect()
}

fn selection_power(
    waiting_for: &WaitingFor,
    selected: &[ObjectId],
    filtered_state: &GameState,
) -> i32 {
    match waiting_for {
        WaitingFor::CrewVehicle {
            eligible_creatures,
            contributions,
            ..
        }
        | WaitingFor::SaddleMount {
            eligible_creatures,
            contributions,
            ..
        } if eligible_creatures.len() == contributions.len() => {
            let contribution_by_id: HashMap<_, _> = eligible_creatures
                .iter()
                .copied()
                .zip(contributions.iter().copied())
                .collect();
            selected
                .iter()
                .map(|object_id| contribution_by_id.get(object_id).copied().unwrap_or(0))
                .sum()
        }
        WaitingFor::KeepWithinTotalPowerChoice { .. } => selected
            .iter()
            .filter_map(|object_id| filtered_state.objects.get(object_id))
            .filter_map(|object| object.power)
            .sum(),
        WaitingFor::WardSacrificeChoice { .. } => {
            super::sacrifice::selected_total_power(filtered_state, selected)
        }
        _ => selected
            .iter()
            .filter_map(|object_id| filtered_state.objects.get(object_id))
            .filter_map(|object| object.power)
            .map(|power| power.max(0))
            .sum(),
    }
}

fn compare_aggregate(comparator: AggregateComparator, lhs: i32, rhs: i32) -> bool {
    match comparator {
        AggregateComparator::GreaterThan => lhs > rhs,
        AggregateComparator::LessThan => lhs < rhs,
        AggregateComparator::AtLeast => lhs >= rhs,
        AggregateComparator::AtMost => lhs <= rhs,
        AggregateComparator::Equal => lhs == rhs,
        AggregateComparator::NotEqual => lhs != rhs,
    }
}

fn selection_progress(
    selection: &SelectionProjection,
    selected: &[ObjectId],
    waiting_for: &WaitingFor,
    filtered_state: &GameState,
) -> InteractionProgress {
    let unique: HashSet<_> = selected.iter().copied().collect();
    let candidate_ids: HashSet<_> = selection.object_ids.iter().copied().collect();
    let all_candidates = selected
        .iter()
        .all(|object_id| candidate_ids.contains(object_id));
    let selected_count = selected.len().min(u32::MAX as usize) as u32;
    let (minimum, maximum, aggregate, constraint_satisfied) = match &selection.constraint {
        SelectionConstraint::Count { min, max } => (
            *min,
            Some(*max),
            None,
            selected_count >= *min && selected_count <= *max,
        ),
        SelectionConstraint::Aggregate {
            function,
            property,
            comparator,
            amount,
        } => {
            let total = if matches!(
                (function, property),
                (
                    InteractionAggregateFunction::Sum,
                    InteractionObjectProperty::Power
                )
            ) {
                selection_power(waiting_for, selected, filtered_state)
            } else {
                super::quantity::aggregate_property_over(
                    filtered_state,
                    selected,
                    match function {
                        InteractionAggregateFunction::Max => AggregateFunction::Max,
                        InteractionAggregateFunction::Min => AggregateFunction::Min,
                        InteractionAggregateFunction::Sum => AggregateFunction::Sum,
                    },
                    match property {
                        InteractionObjectProperty::Power => ObjectProperty::Power,
                        InteractionObjectProperty::Toughness => ObjectProperty::Toughness,
                        InteractionObjectProperty::ManaValue => ObjectProperty::ManaValue,
                        InteractionObjectProperty::ManaSymbolCount { color } => {
                            ObjectProperty::ManaSymbolCount(match color {
                                InteractionManaColor::White => ManaColor::White,
                                InteractionManaColor::Blue => ManaColor::Blue,
                                InteractionManaColor::Black => ManaColor::Black,
                                InteractionManaColor::Red => ManaColor::Red,
                                InteractionManaColor::Green => ManaColor::Green,
                            })
                        }
                    },
                )
            };
            let minimum = u32::from(matches!(
                waiting_for,
                WaitingFor::WardSacrificeChoice { .. }
            ));
            (
                minimum,
                None,
                Some(total),
                selected_count >= minimum && compare_aggregate(*comparator, total, *amount),
            )
        }
        SelectionConstraint::EngineValidatedCount { min, max } => (
            *min,
            Some(*max),
            None,
            selected_count >= *min && selected_count <= *max,
        ),
    };
    InteractionProgress {
        selected: selected_count,
        minimum,
        maximum,
        aggregate,
        confirmable: unique.len() == selected.len() && all_candidates && constraint_satisfied,
    }
}

fn selection_action(
    selection: &SelectionProjection,
    selected: Vec<ObjectId>,
) -> Result<GameAction, InteractionReasonCode> {
    match selection.action {
        SelectionAction::SelectCards => Ok(GameAction::SelectCards { cards: selected }),
        SelectionAction::PilePartition => Ok(GameAction::SubmitPilePartition { pile_a: selected }),
        SelectionAction::Crew { vehicle_id } => Ok(GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: selected,
        }),
        SelectionAction::Station { spacecraft_id } => selected
            .first()
            .copied()
            .map(|creature_id| GameAction::ActivateStation {
                spacecraft_id,
                creature_id: Some(creature_id),
            })
            .ok_or(InteractionReasonCode::ConstraintUnsatisfied),
        SelectionAction::Saddle { mount_id } => Ok(GameAction::SaddleMount {
            mount_id,
            creature_ids: selected,
        }),
        SelectionAction::Harmonize => {
            if selected.len() > 1 {
                Err(InteractionReasonCode::ConstraintUnsatisfied)
            } else {
                Ok(GameAction::HarmonizeTap {
                    creature_id: selected.first().copied(),
                })
            }
        }
        SelectionAction::RingBearer => selected
            .first()
            .copied()
            .map(|target| GameAction::ChooseRingBearer { target })
            .ok_or(InteractionReasonCode::ConstraintUnsatisfied),
        SelectionAction::KeepWithinPower => Ok(GameAction::ChooseKeptCreatures { kept: selected }),
        SelectionAction::KeepExact => Ok(GameAction::ChooseKeptPermanents { kept: selected }),
    }
}

fn selected_objects_from_ids(
    interaction_id: &InteractionId,
    selection: &SelectionProjection,
    choice_ids: &[InteractionChoiceId],
) -> Result<Vec<ObjectId>, InteractionReasonCode> {
    let candidates: HashMap<_, _> = selection
        .object_ids
        .iter()
        .enumerate()
        .map(|(index, object_id)| {
            (
                interaction_choice_id(interaction_id, 's', index),
                *object_id,
            )
        })
        .collect();
    choice_ids
        .iter()
        .map(|choice_id| {
            candidates
                .get(choice_id)
                .copied()
                .ok_or(InteractionReasonCode::UnknownChoice)
        })
        .collect()
}

fn materialize_counter_response(
    interaction_id: &InteractionId,
    projection: &CounterDistributionProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::AssignAmounts { assignments } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    let candidates: HashMap<_, _> = projection
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| (interaction_choice_id(interaction_id, 'a', index), candidate))
        .collect();
    let mut seen = HashSet::new();
    let mut total = 0u32;
    let mut distribution = Vec::with_capacity(assignments.len());
    for assignment in assignments {
        if assignment.amount == 0 || !seen.insert(&assignment.choice_id) {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        let candidate = candidates
            .get(&assignment.choice_id)
            .ok_or(InteractionReasonCode::UnknownChoice)?;
        if assignment.amount > candidate.available {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        total = total
            .checked_add(assignment.amount)
            .ok_or(InteractionReasonCode::PayloadTooLarge)?;
        distribution.push(CounterCostChoice {
            object_id: candidate.object_id,
            counter_type: candidate.counter_type.clone(),
            count: assignment.amount,
        });
    }
    if total != projection.total {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    Ok((
        GameAction::ChooseRemoveCounterCostDistribution { distribution },
        InteractionProgress {
            selected: assignments.len().min(u32::MAX as usize) as u32,
            minimum: projection.total,
            maximum: Some(projection.total),
            aggregate: Some(total as i32),
            confirmable: true,
        },
    ))
}

fn action_advances_interaction(
    state: &GameState,
    actor: PlayerId,
    semantic_owner: PlayerId,
    interaction_id: &InteractionId,
    action: &GameAction,
) -> bool {
    if is_escape_action(action) {
        return false;
    }
    let mut projected = state.clone();
    apply_interaction_for_simulation(&mut projected, actor, semantic_owner, action.clone()).is_ok()
        && !projected
            .active_interaction_slots
            .iter()
            .any(|slot| slot.interaction_id == *interaction_id)
}

fn selection_completion_response(
    state: &GameState,
    waiting_for: &WaitingFor,
    interaction_id: &InteractionId,
    selection: &SelectionProjection,
) -> Option<InteractionResponse> {
    let selected = match &selection.constraint {
        SelectionConstraint::Count { min, .. } => {
            let required = usize::try_from(*min).ok()?;
            let mut seen = HashSet::with_capacity(required.min(selection.object_ids.len()));
            let selected: Vec<_> = selection
                .object_ids
                .iter()
                .copied()
                .filter(|object_id| seen.insert(*object_id))
                .take(required)
                .collect();
            (selected.len() == required).then_some(selected)?
        }
        SelectionConstraint::Aggregate {
            function: InteractionAggregateFunction::Sum,
            property: InteractionObjectProperty::Power,
            comparator: AggregateComparator::AtLeast,
            amount,
        } if matches!(waiting_for, WaitingFor::WardSacrificeChoice { .. }) => {
            crate::ai_support::power_threshold_witness(state, &selection.object_ids, *amount)?
        }
        SelectionConstraint::Aggregate { .. }
        | SelectionConstraint::EngineValidatedCount { .. } => return None,
    };
    let choice_ids = selected
        .iter()
        .map(|object_id| {
            selection
                .object_ids
                .iter()
                .position(|candidate| candidate == object_id)
                .map(|index| interaction_choice_id(interaction_id, 's', index))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(InteractionResponse::Select { choice_ids })
}

fn counter_assignment_completion_response(
    interaction_id: &InteractionId,
    projection: &CounterDistributionProjection,
) -> Option<InteractionResponse> {
    let mut remaining = projection.total;
    let mut assignments = Vec::new();
    for (index, candidate) in projection.candidates.iter().enumerate() {
        let amount = remaining.min(candidate.available);
        remaining -= amount;
        if amount > 0 {
            assignments.push(AmountAssignment {
                choice_id: interaction_choice_id(interaction_id, 'a', index),
                amount,
            });
        }
    }
    (remaining == 0).then_some(InteractionResponse::AssignAmounts { assignments })
}

fn schema_witness_availability(
    authoritative_state: &GameState,
    filtered_state: &GameState,
    viewer: PlayerId,
    semantic_owner: PlayerId,
    interaction_id: &InteractionId,
    response: Option<InteractionResponse>,
) -> InteractionAvailability {
    let Some(response) = response else {
        return InteractionAvailability::InputRequired;
    };
    let authorized_owner = slot_for_submission(authoritative_state, viewer, interaction_id)
        .ok()
        .map(|slot| PlayerId(slot.semantic_owner));
    if authorized_owner != Some(semantic_owner) {
        return InteractionAvailability::InputRequired;
    }
    let Ok((action, _)) = materialize_response(
        authoritative_state,
        filtered_state,
        interaction_id,
        &response,
    ) else {
        return InteractionAvailability::InputRequired;
    };
    if action_advances_interaction(
        authoritative_state,
        viewer,
        semantic_owner,
        interaction_id,
        &action,
    ) {
        InteractionAvailability::ProgressAvailable {
            witness: InteractionSubmission {
                interaction_id: interaction_id.clone(),
                response,
            },
        }
    } else {
        InteractionAvailability::InputRequired
    }
}

fn availability_for_candidates(
    candidates: &[CandidateAction],
    only_escape: bool,
    interaction_id: &InteractionId,
    witness: Option<InteractionResponse>,
) -> InteractionAvailability {
    if only_escape {
        InteractionAvailability::EscapeOnly {
            reason: InteractionReasonCode::CancelOnly,
        }
    } else if let Some(witness) = witness {
        InteractionAvailability::ProgressAvailable {
            witness: InteractionSubmission {
                interaction_id: interaction_id.clone(),
                response: witness,
            },
        }
    } else if candidates.is_empty() {
        InteractionAvailability::Stuck {
            reason: InteractionReasonCode::NoLegalResponse,
        }
    } else {
        InteractionAvailability::Stuck {
            reason: InteractionReasonCode::ConstraintUnsatisfied,
        }
    }
}

fn payload_too_large_opportunity(
    interaction_id: &InteractionId,
) -> (InteractionOpportunity, InteractionAvailability) {
    (
        InteractionOpportunity {
            interaction_id: interaction_id.clone(),
            response: InteractionOpportunityResponse::ExactChoices {
                choices: Vec::new(),
            },
            surfaces: vec![InteractionPresentationSurface::Summary {
                code: InteractionSummaryCode::Decision,
            }],
            progress: InteractionProgress::default(),
        },
        InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::PayloadTooLarge,
        },
    )
}

fn opportunity_for_slot(
    authoritative_state: &GameState,
    filtered_state: &GameState,
    viewer: PlayerId,
    slot: &ActiveInteractionSlot,
) -> (InteractionOpportunity, InteractionAvailability) {
    let semantic_owner = PlayerId(slot.semantic_owner);
    match human_response_model(&filtered_state.waiting_for, semantic_owner) {
        HumanResponseModel::Terminal => payload_too_large_opportunity(&slot.interaction_id),
        HumanResponseModel::TriggerOrder => {
            let projection = match trigger_order_projection(&filtered_state.waiting_for) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("trigger-order model requires trigger projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            let count = projection.count.min(u32::MAX as usize) as u32;
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::Sequence {
                            min: count,
                            max: count,
                            unique: true,
                            include_all: true,
                            engine_validated: false,
                            escape: None,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: trigger_order_choices(
                            &slot.interaction_id,
                            &projection,
                            filtered_state,
                        ),
                    },
                    surfaces: vec![InteractionPresentationSurface::Summary {
                        code: InteractionSummaryCode::Decision,
                    }],
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: count,
                        maximum: Some(count),
                        aggregate: None,
                        confirmable: false,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::CoinFlipSequence => {
            let projection = match coin_flip_projection(&filtered_state.waiting_for) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("coin-flip model requires coin-flip projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            let keep_count = projection.keep_count as u32;
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::Sequence {
                            min: keep_count,
                            max: keep_count,
                            unique: true,
                            include_all: projection.keep_count == projection.candidate_count,
                            engine_validated: false,
                            escape: None,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: coin_flip_choices(&slot.interaction_id, projection),
                    },
                    surfaces: vec![InteractionPresentationSurface::Summary {
                        code: InteractionSummaryCode::Decision,
                    }],
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: keep_count,
                        maximum: Some(keep_count),
                        aggregate: None,
                        confirmable: keep_count == 0,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::TargetSequence => {
            let projection = match target_sequence_projection(&filtered_state.waiting_for) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("target-sequence model requires target projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            let min = projection.min.min(u32::MAX as usize) as u32;
            let max = projection.max.min(u32::MAX as usize) as u32;
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::Sequence {
                            min,
                            max,
                            unique: projection.unique,
                            include_all: projection.min == projection.candidates.len()
                                && projection.max == projection.candidates.len(),
                            engine_validated: false,
                            escape: None,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: target_sequence_choices(
                            &slot.interaction_id,
                            &projection,
                            filtered_state,
                        ),
                    },
                    surfaces: vec![InteractionPresentationSurface::Summary {
                        code: InteractionSummaryCode::Decision,
                    }],
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: min,
                        maximum: Some(max),
                        aggregate: None,
                        confirmable: min == 0,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::CategorySelection => {
            let projection = match category_selection_projection(&filtered_state.waiting_for) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("category model requires category projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            let required = projection.groups.iter().map(|group| group.min).sum();
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::GroupedSequence {
                            groups: projection.groups.clone(),
                            unique: true,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: category_selection_choices(
                            &slot.interaction_id,
                            &projection,
                            filtered_state,
                        ),
                    },
                    surfaces: std::iter::once(InteractionPresentationSurface::Summary {
                        code: InteractionSummaryCode::Decision,
                    })
                    .chain(object_surface(
                        filtered_state,
                        projection.source_id,
                        InteractionRoleCode::Source,
                    ))
                    .collect(),
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: required,
                        maximum: Some(required),
                        aggregate: None,
                        confirmable: required == 0,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::CombatRelations(expected_action) => {
            let projection =
                match combat_relation_projection(&filtered_state.waiting_for, expected_action) {
                    Ok(Some(projection)) => projection,
                    Ok(None) => unreachable!("combat model requires combat projection"),
                    Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
                };
            let maximum = projection.max.min(u32::MAX as usize) as u32;
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::Relations {
                            edges: combat_relation_constraints(&slot.interaction_id, &projection),
                            min: 0,
                            max: maximum,
                            source_constraint: match projection.action {
                                CombatRelationAction::Attackers => {
                                    InteractionRelationSourceConstraint::AtMostOne
                                }
                                CombatRelationAction::Blockers => {
                                    InteractionRelationSourceConstraint::EngineValidated
                                }
                            },
                            allow_groups: projection.action == CombatRelationAction::Attackers,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: combat_relation_choices(
                            &slot.interaction_id,
                            &projection,
                            filtered_state,
                        ),
                    },
                    surfaces: vec![InteractionPresentationSurface::Selection {
                        intent: match projection.action {
                            CombatRelationAction::Attackers => InteractionIntentCode::Attack,
                            CombatRelationAction::Blockers => InteractionIntentCode::Block,
                        },
                        constraint: SelectionConstraint::EngineValidatedCount {
                            min: 0,
                            max: maximum,
                        },
                        confirm: ConfirmSemantics::Explicit,
                    }],
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: 0,
                        maximum: Some(maximum),
                        aggregate: None,
                        confirmable: false,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::ManaGroups(expected_action) => {
            let projection =
                match mana_group_projection(&filtered_state.waiting_for, expected_action) {
                    Ok(Some(projection)) => projection,
                    Ok(None) => unreachable!("mana model requires mana projection"),
                    Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
                };
            let required = projection.groups.iter().map(|group| group.min).sum();
            let escape = projection
                .allow_cancel
                .then(|| interaction_choice_id(&slot.interaction_id, 'e', 0));
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::ManaGroups {
                            groups: projection.groups.clone(),
                            max_batch: projection.max_batch,
                            escape,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: mana_group_choices(&slot.interaction_id, &projection),
                    },
                    surfaces: std::iter::once(InteractionPresentationSurface::Summary {
                        code: InteractionSummaryCode::Decision,
                    })
                    .chain(projection.source_id.and_then(|source_id| {
                        object_surface(filtered_state, source_id, InteractionRoleCode::Source)
                    }))
                    .collect(),
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: required,
                        maximum: Some(required),
                        aggregate: None,
                        confirmable: required == 0,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::ModeSequence => {
            let projection = match mode_sequence_projection(&filtered_state.waiting_for) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("mode model requires mode projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            let min = projection.min.min(u32::MAX as usize) as u32;
            let max = projection.max.min(u32::MAX as usize) as u32;
            let escape = projection
                .allow_cancel
                .then(|| interaction_choice_id(&slot.interaction_id, 'e', 0));
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::Sequence {
                            min,
                            max,
                            unique: projection.unique,
                            include_all: false,
                            engine_validated: true,
                            escape,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: mode_sequence_choices(&slot.interaction_id, &projection),
                    },
                    surfaces: std::iter::once(InteractionPresentationSurface::Summary {
                        code: InteractionSummaryCode::Decision,
                    })
                    .chain(object_surface(
                        filtered_state,
                        projection.source_id,
                        InteractionRoleCode::Source,
                    ))
                    .collect(),
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: min,
                        maximum: Some(max),
                        aggregate: None,
                        confirmable: min == 0,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::OutsideSelection => {
            let projection = match outside_selection_projection(&filtered_state.waiting_for) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("outside-game model requires outside-game projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            let min = projection.min.min(u32::MAX as usize) as u32;
            let max = projection.max.min(u32::MAX as usize) as u32;
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::Select {
                            constraint: SelectionConstraint::Count { min, max },
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: outside_selection_choices(
                            &slot.interaction_id,
                            &projection,
                            filtered_state,
                        ),
                    },
                    surfaces: std::iter::once(InteractionPresentationSurface::Selection {
                        intent: InteractionIntentCode::Choose,
                        constraint: SelectionConstraint::Count { min, max },
                        confirm: ConfirmSemantics::Explicit,
                    })
                    .chain(object_surface(
                        filtered_state,
                        projection.source_id,
                        InteractionRoleCode::Source,
                    ))
                    .collect(),
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: min,
                        maximum: Some(max),
                        aggregate: None,
                        confirmable: min == 0,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::TextChoice => {
            let projection = match text_choice_projection(&filtered_state.waiting_for) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("text model requires text projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::Text {
                            allow_arbitrary: projection.allow_arbitrary,
                            max_len: MAX_INTERACTION_STRING_LEN as u32,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: text_choice_suggestions(&slot.interaction_id, &projection),
                    },
                    surfaces: std::iter::once(InteractionPresentationSurface::Summary {
                        code: InteractionSummaryCode::Decision,
                    })
                    .chain(projection.source_name.as_ref().map(|source_name| {
                        InteractionPresentationSurface::Value {
                            role: InteractionRoleCode::Source,
                            index: None,
                            value: source_name.clone(),
                        }
                    }))
                    .collect(),
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: 1,
                        maximum: Some(1),
                        aggregate: None,
                        confirmable: false,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::ShortcutReply => {
            let projection = shortcut_reply_projection(&filtered_state.waiting_for)
                .expect("shortcut-reply model requires reply projection");
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::ShortcutReply {
                            min_iteration: projection.min_iteration,
                            max_iteration: projection.max_iteration,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: Vec::new(),
                    },
                    surfaces: vec![
                        InteractionPresentationSurface::ShortcutResponse {
                            response: InteractionShortcutResponseCode::Accept,
                        },
                        InteractionPresentationSurface::ShortcutResponse {
                            response: InteractionShortcutResponseCode::Shorten,
                        },
                        InteractionPresentationSurface::Amount {
                            min: projection.min_iteration,
                            max: projection.max_iteration,
                            total: None,
                        },
                    ],
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: 1,
                        maximum: Some(1),
                        aggregate: None,
                        confirmable: false,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::DirectChoices => {
            let projection = match direct_choice_projection(
                &filtered_state.waiting_for,
                filtered_state,
                semantic_owner,
            ) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("direct-choice model requires direct projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::ExactChoices {
                        choices: direct_choices(&slot.interaction_id, &projection, filtered_state),
                    },
                    surfaces: vec![InteractionPresentationSurface::Summary {
                        code: InteractionSummaryCode::Decision,
                    }],
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: 1,
                        maximum: Some(1),
                        aggregate: None,
                        confirmable: false,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::SideboardPartition => {
            let projection = match sideboard_projection(
                &filtered_state.waiting_for,
                filtered_state,
                semantic_owner,
            ) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("sideboard model requires sideboard projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::DeckPartition {
                            main_total: projection.main_total,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: sideboard_choices(&slot.interaction_id, &projection),
                    },
                    surfaces: vec![InteractionPresentationSurface::Amount {
                        min: projection.main_total,
                        max: projection.main_total,
                        total: Some(projection.main_total),
                    }],
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: projection.main_total,
                        maximum: Some(projection.main_total),
                        aggregate: Some(0),
                        confirmable: projection.main_total == 0,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::NumberRange(expected_action) => {
            let projection = number_projection(&filtered_state.waiting_for)
                .filter(|projection| projection.action == expected_action)
                .expect("number model requires a matching projection");
            let availability = schema_witness_availability(
                authoritative_state,
                filtered_state,
                viewer,
                semantic_owner,
                &slot.interaction_id,
                Some(InteractionResponse::Number {
                    value: projection.min,
                }),
            );
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::Number {
                            min: projection.min,
                            max: projection.max,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: Vec::new(),
                    },
                    surfaces: vec![
                        InteractionPresentationSurface::Summary {
                            code: InteractionSummaryCode::Decision,
                        },
                        InteractionPresentationSurface::Amount {
                            min: projection.min,
                            max: projection.max,
                            total: None,
                        },
                    ],
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: projection.min,
                        maximum: Some(projection.max),
                        aggregate: None,
                        confirmable: false,
                    },
                },
                availability,
            )
        }
        HumanResponseModel::LoopShortcut => {
            let projection = match loop_shortcut_projection(&filtered_state.waiting_for) {
                Ok(projection) => projection,
                Err(reason) => {
                    return (
                        InteractionOpportunity {
                            interaction_id: slot.interaction_id.clone(),
                            response: InteractionOpportunityResponse::ExactChoices {
                                choices: Vec::new(),
                            },
                            surfaces: vec![InteractionPresentationSurface::Summary {
                                code: InteractionSummaryCode::Decision,
                            }],
                            progress: InteractionProgress::default(),
                        },
                        InteractionAvailability::Unsupported { reason },
                    );
                }
            };
            let candidates =
                loop_shortcut_choices(&slot.interaction_id, &projection, filtered_state);
            let points = loop_shortcut_points(&slot.interaction_id, &projection);
            let pin_minimum = projection
                .points
                .iter()
                .filter(|point| !point.read_only)
                .map(|point| point.min)
                .sum::<u32>();
            let pin_maximum = projection
                .points
                .iter()
                .filter(|point| !point.read_only)
                .map(|point| point.max)
                .sum::<u32>();
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::Shortcut {
                            count: projection.count,
                            points,
                            allow_decline: true,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates,
                    },
                    surfaces: vec![InteractionPresentationSurface::Summary {
                        code: InteractionSummaryCode::Decision,
                    }],
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: 1 + pin_minimum,
                        maximum: Some(1 + pin_maximum),
                        aggregate: None,
                        confirmable: false,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::AssignAmounts => {
            let projection = match counter_distribution_projection(
                &filtered_state.waiting_for,
                filtered_state,
            ) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("amount model requires amount projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            let availability = schema_witness_availability(
                authoritative_state,
                filtered_state,
                viewer,
                semantic_owner,
                &slot.interaction_id,
                counter_assignment_completion_response(&slot.interaction_id, &projection),
            );
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::AssignAmounts {
                            min_total: projection.total,
                            max_total: projection.total,
                            exact_total: Some(projection.total),
                        },
                        candidates: counter_assignment_choices(
                            &slot.interaction_id,
                            &projection,
                            filtered_state,
                        ),
                    },
                    surfaces: vec![InteractionPresentationSurface::Amount {
                        min: projection.total,
                        max: projection.total,
                        total: Some(projection.total),
                    }],
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: projection.total,
                        maximum: Some(projection.total),
                        aggregate: Some(0),
                        confirmable: false,
                    },
                },
                availability,
            )
        }
        HumanResponseModel::AmountAssignments => {
            let projection = match amount_assignment_projection(&filtered_state.waiting_for) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("amount model requires amount projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            let source = match &filtered_state.waiting_for {
                WaitingFor::AssignBlockerDamage { blocker_id, .. }
                | WaitingFor::MoveCountersDistribution {
                    source_id: blocker_id,
                    ..
                }
                | WaitingFor::RemoveCountersChoice {
                    source_id: blocker_id,
                    ..
                } => object_surface(filtered_state, *blocker_id, InteractionRoleCode::Source),
                WaitingFor::DistributeAmong { .. } => None,
                _ => unreachable!("amount model matched the wrong waiting state"),
            };
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::AssignAmounts {
                            min_total: projection.min_total,
                            max_total: projection.max_total,
                            exact_total: projection.exact_total,
                        },
                        candidates: amount_assignment_choices(
                            &slot.interaction_id,
                            &projection.candidates,
                            projection.exact_total,
                            filtered_state,
                        ),
                    },
                    surfaces: std::iter::once(InteractionPresentationSurface::Amount {
                        min: projection.min_total,
                        max: projection.max_total,
                        total: projection.exact_total,
                    })
                    .chain(source)
                    .collect(),
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: projection.min_total,
                        maximum: Some(projection.max_total),
                        aggregate: Some(0),
                        confirmable: projection.min_total == 0,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::DamageAssignments => {
            let projection = match damage_assignment_projection(&filtered_state.waiting_for) {
                Ok(Some(projection)) => projection,
                Ok(None) => unreachable!("damage model requires damage projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            let source = match filtered_state.waiting_for {
                WaitingFor::AssignCombatDamage { attacker_id, .. } => {
                    object_surface(filtered_state, attacker_id, InteractionRoleCode::Source)
                }
                _ => unreachable!("damage model matched the wrong waiting state"),
            };
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::AssignDamage {
                            total: projection.total,
                            modes: projection.modes.clone(),
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: amount_assignment_choices(
                            &slot.interaction_id,
                            &projection.candidates,
                            Some(projection.total),
                            filtered_state,
                        ),
                    },
                    surfaces: std::iter::once(InteractionPresentationSurface::Amount {
                        min: projection.total,
                        max: projection.total,
                        total: Some(projection.total),
                    })
                    .chain(source)
                    .collect(),
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: projection.total,
                        maximum: Some(projection.total),
                        aggregate: Some(0),
                        confirmable: false,
                    },
                },
                InteractionAvailability::InputRequired,
            )
        }
        HumanResponseModel::Select => {
            let selection = match selection_projection(
                &filtered_state.waiting_for,
                filtered_state,
                semantic_owner,
            ) {
                Ok(Some(selection)) => selection,
                Ok(None) => unreachable!("selection model requires selection projection"),
                Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
            };
            let progress =
                selection_progress(&selection, &[], &filtered_state.waiting_for, filtered_state);
            let availability = schema_witness_availability(
                authoritative_state,
                filtered_state,
                viewer,
                semantic_owner,
                &slot.interaction_id,
                selection_completion_response(
                    filtered_state,
                    &filtered_state.waiting_for,
                    &slot.interaction_id,
                    &selection,
                ),
            );
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::Select {
                            constraint: selection.constraint.clone(),
                            confirm: selection.confirm,
                        },
                        candidates: selection_choices(
                            &slot.interaction_id,
                            &selection,
                            filtered_state,
                        ),
                    },
                    surfaces: vec![
                        InteractionPresentationSurface::Summary {
                            code: InteractionSummaryCode::Decision,
                        },
                        InteractionPresentationSurface::Selection {
                            intent: selection.intent,
                            constraint: selection.constraint,
                            confirm: selection.confirm,
                        },
                    ]
                    .into_iter()
                    .chain(selection.source_id.and_then(|source| {
                        object_surface(filtered_state, source, InteractionRoleCode::Source)
                    }))
                    .collect(),
                    progress,
                },
                availability,
            )
        }
        HumanResponseModel::ExactCandidates(AuditedExactCandidates) => {
            let candidates =
                match actor_candidates(authoritative_state, PlayerId(slot.semantic_owner)) {
                    Ok(candidates) => candidates,
                    Err(_) => return payload_too_large_opportunity(&slot.interaction_id),
                };
            let choices = exact_choices(&slot.interaction_id, &candidates, filtered_state);
            let opportunity = InteractionOpportunity {
                interaction_id: slot.interaction_id.clone(),
                response: InteractionOpportunityResponse::ExactChoices { choices },
                surfaces: vec![InteractionPresentationSurface::Summary {
                    code: InteractionSummaryCode::Decision,
                }],
                progress: InteractionProgress::default(),
            };
            if bound_outbound_opportunity(&opportunity).is_err() {
                return payload_too_large_opportunity(&slot.interaction_id);
            }
            let progress_candidate = candidates.iter().find(|candidate| {
                action_advances_interaction(
                    authoritative_state,
                    viewer,
                    PlayerId(slot.semantic_owner),
                    &slot.interaction_id,
                    &candidate.action,
                )
            });
            let only_escape = !candidates.is_empty()
                && candidates
                    .iter()
                    .all(|candidate| is_escape_action(&candidate.action));
            let witness = progress_candidate.and_then(|candidate| {
                candidates
                    .iter()
                    .position(|item| item.action == candidate.action)
                    .map(|index| InteractionResponse::Choose {
                        choice_id: interaction_choice_id(&slot.interaction_id, 'c', index),
                    })
            });
            let availability = availability_for_candidates(
                &candidates,
                only_escape,
                &slot.interaction_id,
                witness,
            );
            (opportunity, availability)
        }
    }
}

/// Build an actor-scoped, viewer-safe interaction projection. Authorization and
/// capability identity are read only from `authoritative_state`; every object,
/// card, zone, and presentation surface is read only from `filtered_state`.
pub fn derive_viewer_interaction(
    authoritative_state: &GameState,
    filtered_state: &GameState,
    viewer: PlayerId,
) -> ViewerInteraction {
    debug_assert_interaction_consistency(authoritative_state);
    let authorized_submitters = turn_control::authorized_submitters(authoritative_state);
    let can_submit = authorized_submitters.contains(&viewer);
    let kind = waiting_for_kind(&authoritative_state.waiting_for);
    if kind.terminal {
        return ViewerInteraction {
            waiting_for_kind: kind,
            authorized_submitters: Vec::new(),
            can_submit: false,
            auto_pass_recommended: false,
            opportunities: Vec::new(),
            availability: InteractionAvailability::Terminal {
                outcome: InteractionOutcomeCode::Terminal,
            },
        };
    }
    if !can_submit {
        return ViewerInteraction {
            waiting_for_kind: kind,
            authorized_submitters: authorized_submitters
                .into_iter()
                .map(|player| player.0)
                .collect(),
            can_submit: false,
            auto_pass_recommended: false,
            opportunities: Vec::new(),
            availability: InteractionAvailability::Waiting,
        };
    }
    if authoritative_state
        .interaction_session_id
        .as_ref()
        .is_none_or(|session| !interaction_session_is_valid(session))
    {
        return ViewerInteraction {
            waiting_for_kind: kind,
            authorized_submitters: authorized_submitters
                .into_iter()
                .map(|player| player.0)
                .collect(),
            can_submit: true,
            auto_pass_recommended: false,
            opportunities: Vec::new(),
            availability: InteractionAvailability::Unsupported {
                reason: InteractionReasonCode::AuthorityUnbound,
            },
        };
    }
    if !interaction_serial_is_valid(&authoritative_state.next_interaction_serial) {
        return ViewerInteraction {
            waiting_for_kind: kind,
            authorized_submitters: authorized_submitters
                .into_iter()
                .map(|player| player.0)
                .collect(),
            can_submit: true,
            auto_pass_recommended: false,
            opportunities: Vec::new(),
            availability: InteractionAvailability::Unsupported {
                reason: InteractionReasonCode::InvalidAuthorityState,
            },
        };
    }

    let slots: Vec<_> = authoritative_state
        .active_interaction_slots
        .iter()
        .filter(|slot| {
            turn_control::authorized_submitter_for_player(
                authoritative_state,
                PlayerId(slot.semantic_owner),
            ) == viewer
        })
        .collect();
    if slots.len() > MAX_INTERACTION_LIST_LEN {
        return ViewerInteraction {
            waiting_for_kind: kind,
            authorized_submitters: authorized_submitters
                .into_iter()
                .map(|player| player.0)
                .collect(),
            can_submit: true,
            auto_pass_recommended: false,
            opportunities: Vec::new(),
            availability: InteractionAvailability::Unsupported {
                reason: InteractionReasonCode::PayloadTooLarge,
            },
        };
    }
    let mut opportunities = Vec::with_capacity(slots.len());
    let mut first_progress = None;
    let mut first_fallback = None;
    let default_availability = InteractionAvailability::Stuck {
        reason: InteractionReasonCode::NoLegalResponse,
    };
    for slot in slots {
        let (mut opportunity, mut slot_availability) =
            opportunity_for_slot(authoritative_state, filtered_state, viewer, slot);
        if bound_outbound_opportunity(&opportunity).is_err() {
            (opportunity, slot_availability) = payload_too_large_opportunity(&slot.interaction_id);
        }
        if matches!(
            slot_availability,
            InteractionAvailability::ProgressAvailable { .. }
        ) {
            if first_progress.is_none() {
                first_progress = Some(slot_availability);
            }
        } else if first_fallback.is_none() {
            first_fallback = Some(slot_availability);
        }
        opportunities.push(opportunity);
    }
    let availability = first_progress
        .or(first_fallback)
        .unwrap_or(default_availability);
    let mut view = ViewerInteraction {
        waiting_for_kind: kind,
        authorized_submitters: authorized_submitters
            .into_iter()
            .map(|player| player.0)
            .collect(),
        can_submit: true,
        auto_pass_recommended: matches!(
            authoritative_state.waiting_for,
            WaitingFor::Priority { .. }
        ) && authoritative_state.auto_pass.contains_key(&viewer),
        opportunities,
        availability,
    };
    if bound_outbound_view(&view).is_err() {
        view.opportunities.clear();
        view.availability = InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::PayloadTooLarge,
        };
    }
    view
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionSubmitError {
    pub code: InteractionReasonCode,
}

impl From<InteractionReasonCode> for InteractionSubmitError {
    fn from(code: InteractionReasonCode) -> Self {
        Self { code }
    }
}

fn bound_string(value: &str) -> Result<(), InteractionReasonCode> {
    if value.len() > MAX_INTERACTION_STRING_LEN {
        Err(InteractionReasonCode::PayloadTooLarge)
    } else {
        Ok(())
    }
}

#[derive(Debug, Default)]
struct OutboundBudget {
    entries: usize,
    string_bytes: usize,
}

impl OutboundBudget {
    fn list(&mut self, len: usize) -> Result<(), InteractionReasonCode> {
        if len > MAX_INTERACTION_LIST_LEN {
            return Err(InteractionReasonCode::PayloadTooLarge);
        }
        self.entries = self
            .entries
            .checked_add(len)
            .filter(|entries| *entries <= MAX_INTERACTION_LIST_LEN)
            .ok_or(InteractionReasonCode::PayloadTooLarge)?;
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<(), InteractionReasonCode> {
        bound_string(value)?;
        self.string_bytes = self
            .string_bytes
            .checked_add(value.len())
            .filter(|bytes| {
                *bytes <= MAX_INTERACTION_LIST_LEN.saturating_mul(MAX_INTERACTION_STRING_LEN)
            })
            .ok_or(InteractionReasonCode::PayloadTooLarge)?;
        Ok(())
    }
}

fn bound_outbound_surface(
    surface: &InteractionPresentationSurface,
    budget: &mut OutboundBudget,
) -> Result<(), InteractionReasonCode> {
    match surface {
        InteractionPresentationSurface::Object {
            reference, name, ..
        } => {
            budget.string(reference)?;
            if let Some(name) = name {
                budget.string(name)?;
            }
        }
        InteractionPresentationSurface::Value { value, .. }
        | InteractionPresentationSurface::Counter {
            counter_type: value,
            ..
        } => budget.string(value)?,
        InteractionPresentationSurface::Mana { symbols, .. } => {
            budget.list(symbols.len())?;
            for symbol in symbols {
                budget.string(symbol)?;
            }
        }
        InteractionPresentationSurface::Summary { .. }
        | InteractionPresentationSurface::Action { .. }
        | InteractionPresentationSurface::Player { .. }
        | InteractionPresentationSurface::Zone { .. }
        | InteractionPresentationSurface::Selection { .. }
        | InteractionPresentationSurface::Amount { .. }
        | InteractionPresentationSurface::ShortcutResponse { .. } => {}
    }
    Ok(())
}

fn bound_outbound_choices(
    choices: &[InteractionChoice],
    budget: &mut OutboundBudget,
) -> Result<(), InteractionReasonCode> {
    budget.list(choices.len())?;
    for choice in choices {
        budget.string(choice.id.as_str())?;
        budget.list(choice.surfaces.len())?;
        for surface in &choice.surfaces {
            bound_outbound_surface(surface, budget)?;
        }
    }
    Ok(())
}

fn bound_outbound_spec(
    spec: &InteractionResponseSpec,
    budget: &mut OutboundBudget,
) -> Result<(), InteractionReasonCode> {
    match spec {
        InteractionResponseSpec::AssignDamage { modes, .. } => budget.list(modes.len())?,
        InteractionResponseSpec::Sequence { escape, .. } => {
            if let Some(escape) = escape {
                budget.string(escape.as_str())?;
            }
        }
        InteractionResponseSpec::GroupedSequence { groups, .. } => budget.list(groups.len())?,
        InteractionResponseSpec::ManaGroups { groups, escape, .. } => {
            budget.list(groups.len())?;
            if let Some(escape) = escape {
                budget.string(escape.as_str())?;
            }
        }
        InteractionResponseSpec::Relations { edges, .. } => {
            budget.list(edges.len())?;
            for edge in edges {
                budget.string(edge.source_id.as_str())?;
                budget.list(edge.target_ids.len())?;
                for target_id in &edge.target_ids {
                    budget.string(target_id.as_str())?;
                }
            }
        }
        InteractionResponseSpec::Shortcut { points, .. } => {
            budget.list(points.len())?;
            for point in points {
                budget.list(point.candidate_ids.len())?;
                for candidate_id in &point.candidate_ids {
                    budget.string(candidate_id.as_str())?;
                }
            }
        }
        InteractionResponseSpec::Select { .. }
        | InteractionResponseSpec::AssignAmounts { .. }
        | InteractionResponseSpec::Text { .. }
        | InteractionResponseSpec::DeckPartition { .. }
        | InteractionResponseSpec::Number { .. }
        | InteractionResponseSpec::ShortcutReply { .. } => {}
    }
    Ok(())
}

fn bound_outbound_response(
    response: &InteractionResponse,
    budget: &mut OutboundBudget,
) -> Result<(), InteractionReasonCode> {
    match response {
        InteractionResponse::Choose { choice_id } => budget.string(choice_id.as_str())?,
        InteractionResponse::Select { choice_ids }
        | InteractionResponse::Sequence { choice_ids }
        | InteractionResponse::ManaGroups { choice_ids, .. } => {
            budget.list(choice_ids.len())?;
            for choice_id in choice_ids {
                budget.string(choice_id.as_str())?;
            }
        }
        InteractionResponse::AssignAmounts { assignments }
        | InteractionResponse::AssignDamage { assignments, .. }
        | InteractionResponse::DeckPartition { main: assignments } => {
            budget.list(assignments.len())?;
            for assignment in assignments {
                budget.string(assignment.choice_id.as_str())?;
            }
        }
        InteractionResponse::Relations { relations } => {
            budget.list(relations.len())?;
            for relation in relations {
                budget.string(relation.source_id.as_str())?;
                budget.string(relation.target_id.as_str())?;
            }
        }
        InteractionResponse::Shortcut { pins, .. } => {
            budget.list(pins.len())?;
            for pin in pins {
                budget.list(pin.choice_ids.len())?;
                for choice_id in &pin.choice_ids {
                    budget.string(choice_id.as_str())?;
                }
            }
        }
        InteractionResponse::Text { value } => budget.string(value)?,
        InteractionResponse::Number { .. } | InteractionResponse::ShortcutReply { .. } => {}
    }
    Ok(())
}

fn bound_outbound_opportunity(
    opportunity: &InteractionOpportunity,
) -> Result<(), InteractionReasonCode> {
    let mut budget = OutboundBudget::default();
    bound_outbound_opportunity_with_budget(opportunity, &mut budget)
}

fn bound_outbound_opportunity_with_budget(
    opportunity: &InteractionOpportunity,
    budget: &mut OutboundBudget,
) -> Result<(), InteractionReasonCode> {
    budget.string(opportunity.interaction_id.as_str())?;
    budget.list(opportunity.surfaces.len())?;
    for surface in &opportunity.surfaces {
        bound_outbound_surface(surface, budget)?;
    }
    match &opportunity.response {
        InteractionOpportunityResponse::ExactChoices { choices } => {
            bound_outbound_choices(choices, budget)?;
        }
        InteractionOpportunityResponse::Schema { spec, candidates } => {
            bound_outbound_spec(spec, budget)?;
            bound_outbound_choices(candidates, budget)?;
        }
    }
    Ok(())
}

fn bound_outbound_view(view: &ViewerInteraction) -> Result<(), InteractionReasonCode> {
    let mut budget = OutboundBudget::default();
    budget.list(view.authorized_submitters.len())?;
    budget.list(view.opportunities.len())?;
    for opportunity in &view.opportunities {
        bound_outbound_opportunity_with_budget(opportunity, &mut budget)?;
    }
    if let InteractionAvailability::ProgressAvailable { witness } = &view.availability {
        budget.string(witness.interaction_id.as_str())?;
        bound_outbound_response(&witness.response, &mut budget)?;
    }
    Ok(())
}

fn bound_ids(ids: &[InteractionChoiceId]) -> Result<(), InteractionReasonCode> {
    if ids.len() > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    for id in ids {
        bound_string(id.as_str())?;
    }
    Ok(())
}

fn validate_response_bounds(response: &InteractionResponse) -> Result<(), InteractionReasonCode> {
    match response {
        InteractionResponse::Choose { choice_id } => bound_string(choice_id.as_str()),
        InteractionResponse::Select { choice_ids } => bound_ids(choice_ids),
        InteractionResponse::AssignAmounts { assignments }
        | InteractionResponse::AssignDamage { assignments, .. }
        | InteractionResponse::DeckPartition { main: assignments } => {
            if assignments.len() > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            for assignment in assignments {
                bound_string(assignment.choice_id.as_str())?;
            }
            Ok(())
        }
        InteractionResponse::Sequence { choice_ids }
        | InteractionResponse::ManaGroups { choice_ids, .. } => bound_ids(choice_ids),
        InteractionResponse::Relations { relations } => {
            if relations.len() > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
            for relation in relations {
                bound_string(relation.source_id.as_str())?;
                bound_string(relation.target_id.as_str())?;
            }
            Ok(())
        }
        InteractionResponse::Text { value } => bound_string(value),
        InteractionResponse::Shortcut { pins, .. } => {
            let mut budget = OutboundBudget::default();
            budget.list(pins.len())?;
            for pin in pins {
                budget.list(pin.choice_ids.len())?;
                for choice_id in &pin.choice_ids {
                    budget.string(choice_id.as_str())?;
                }
            }
            Ok(())
        }
        InteractionResponse::Number { .. } | InteractionResponse::ShortcutReply { .. } => Ok(()),
    }
}

fn slot_for_submission<'a>(
    state: &'a GameState,
    actor: PlayerId,
    interaction_id: &InteractionId,
) -> Result<&'a ActiveInteractionSlot, InteractionReasonCode> {
    if state
        .interaction_session_id
        .as_ref()
        .is_none_or(|session| !interaction_session_is_valid(session))
        || !interaction_serial_is_valid(&state.next_interaction_serial)
    {
        return Err(InteractionReasonCode::InvalidAuthorityState);
    }
    let slot = state
        .active_interaction_slots
        .iter()
        .find(|slot| slot.interaction_id == *interaction_id)
        .ok_or(InteractionReasonCode::StaleInteraction)?;
    let authorized =
        turn_control::authorized_submitter_for_player(state, PlayerId(slot.semantic_owner));
    if authorized != actor {
        return Err(InteractionReasonCode::NotAuthorized);
    }
    Ok(slot)
}

/// CR 603.3b: preserve the controller's complete submitted trigger permutation.
fn materialize_trigger_order_response(
    interaction_id: &InteractionId,
    projection: &TriggerOrderProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::Sequence { choice_ids } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    if choice_ids.len() != projection.count {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let mut seen = HashSet::with_capacity(choice_ids.len());
    let mut order = Vec::with_capacity(choice_ids.len());
    for choice_id in choice_ids {
        let index = (0..projection.count)
            .find(|index| interaction_choice_id(interaction_id, 'q', *index) == *choice_id)
            .ok_or(InteractionReasonCode::UnknownChoice)?;
        if !seen.insert(index) {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        order.push(index);
    }
    Ok((
        GameAction::OrderTriggers { order },
        InteractionProgress {
            selected: choice_ids.len().min(u32::MAX as usize) as u32,
            minimum: projection.count.min(u32::MAX as usize) as u32,
            maximum: Some(projection.count.min(u32::MAX as usize) as u32),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn materialize_coin_flip_response(
    interaction_id: &InteractionId,
    projection: CoinFlipProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::Sequence { choice_ids } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    if choice_ids.len() != projection.keep_count {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let mut seen = HashSet::with_capacity(choice_ids.len());
    let keep_indices = choice_ids
        .iter()
        .map(|choice_id| {
            let index = (0..projection.candidate_count)
                .find(|index| interaction_choice_id(interaction_id, 'f', *index) == *choice_id)
                .ok_or(InteractionReasonCode::UnknownChoice)?;
            if !seen.insert(index) {
                return Err(InteractionReasonCode::ConstraintUnsatisfied);
            }
            Ok(index)
        })
        .collect::<Result<_, _>>()?;
    Ok((
        GameAction::SelectCoinFlips { keep_indices },
        InteractionProgress {
            selected: projection.keep_count as u32,
            minimum: projection.keep_count as u32,
            maximum: Some(projection.keep_count as u32),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn materialize_target_sequence_response(
    interaction_id: &InteractionId,
    projection: &TargetSequenceProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::Sequence { choice_ids } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    if choice_ids.len() < projection.min || choice_ids.len() > projection.max {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let mut seen = HashSet::with_capacity(choice_ids.len());
    let targets = choice_ids
        .iter()
        .map(|choice_id| {
            let index = (0..projection.candidates.len())
                .find(|index| interaction_choice_id(interaction_id, 't', *index) == *choice_id)
                .ok_or(InteractionReasonCode::UnknownChoice)?;
            if projection.unique && !seen.insert(index) {
                return Err(InteractionReasonCode::ConstraintUnsatisfied);
            }
            Ok(projection.candidates[index].clone())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let action = match projection.action {
        TargetSequenceAction::ChooseTarget => match targets.as_slice() {
            [] => GameAction::ChooseTarget { target: None },
            [target] => GameAction::ChooseTarget {
                target: Some(target.clone()),
            },
            _ => return Err(InteractionReasonCode::ConstraintUnsatisfied),
        },
        TargetSequenceAction::SelectObjects => GameAction::SelectCards {
            cards: targets
                .iter()
                .map(|target| match target {
                    TargetRef::Object(object_id) => Ok(*object_id),
                    _ => Err(InteractionReasonCode::MalformedResponse),
                })
                .collect::<Result<_, _>>()?,
        },
        TargetSequenceAction::SelectTargets => GameAction::SelectTargets { targets },
        TargetSequenceAction::Retarget => GameAction::RetargetSpell {
            new_targets: targets,
        },
    };
    Ok((
        action,
        InteractionProgress {
            selected: choice_ids.len().min(u32::MAX as usize) as u32,
            minimum: projection.min.min(u32::MAX as usize) as u32,
            maximum: Some(projection.max.min(u32::MAX as usize) as u32),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn materialize_category_selection_response(
    interaction_id: &InteractionId,
    projection: &CategorySelectionProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::Sequence { choice_ids } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    let mut seen = HashSet::with_capacity(choice_ids.len());
    let mut choices = vec![None; projection.groups.len()];
    for choice_id in choice_ids {
        let index = (0..projection.candidates.len())
            .find(|index| interaction_choice_id(interaction_id, 'g', *index) == *choice_id)
            .ok_or(InteractionReasonCode::UnknownChoice)?;
        if !seen.insert(index) {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        let candidate = &projection.candidates[index];
        if choices[candidate.group]
            .replace(candidate.object_id)
            .is_some()
        {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
    }
    for constraint in &projection.groups {
        let selected = u32::from(choices[constraint.group as usize].is_some());
        if selected < constraint.min || selected > constraint.max {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
    }
    let selected = choice_ids.len().min(u32::MAX as usize) as u32;
    let required = projection.groups.iter().map(|group| group.min).sum();
    Ok((
        GameAction::SelectCategoryPermanents { choices },
        InteractionProgress {
            selected,
            minimum: required,
            maximum: Some(required),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn materialize_combat_relation_response(
    interaction_id: &InteractionId,
    projection: &CombatRelationProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::Relations { relations } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    if relations.len() > projection.max {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let source_indices = (0..projection.sources.len())
        .map(|index| (interaction_choice_id(interaction_id, 'r', index), index))
        .collect::<HashMap<_, _>>();
    let target_indices = (0..projection.targets.len())
        .map(|index| (interaction_choice_id(interaction_id, 'd', index), index))
        .collect::<HashMap<_, _>>();
    let mut seen_relations = HashSet::with_capacity(relations.len());
    let mut seen_sources = HashSet::with_capacity(relations.len());
    let mut decoded = Vec::with_capacity(relations.len());
    for relation in relations {
        let source_index = *source_indices
            .get(&relation.source_id)
            .ok_or(InteractionReasonCode::UnknownChoice)?;
        let target_index = *target_indices
            .get(&relation.target_id)
            .ok_or(InteractionReasonCode::UnknownChoice)?;
        if !projection.legal_target_indices[source_index].contains(&target_index)
            || !seen_relations.insert((source_index, target_index))
        {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        if projection.action == CombatRelationAction::Attackers
            && !seen_sources.insert(source_index)
        {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        if projection.action == CombatRelationAction::Blockers && relation.group.is_some() {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        decoded.push((source_index, target_index, relation.group));
    }
    let action = match projection.action {
        CombatRelationAction::Attackers => {
            let mut bands = BTreeMap::<u32, Vec<ObjectId>>::new();
            let attacks = decoded
                .iter()
                .map(|(source_index, target_index, group)| {
                    let CombatRelationTarget::Attack(target) = projection.targets[*target_index]
                    else {
                        return Err(InteractionReasonCode::MalformedResponse);
                    };
                    let attacker = projection.sources[*source_index];
                    if let Some(group) = group {
                        bands.entry(*group).or_default().push(attacker);
                    }
                    Ok((attacker, target))
                })
                .collect::<Result<_, _>>()?;
            GameAction::DeclareAttackers {
                attacks,
                bands: bands.into_values().collect(),
            }
        }
        CombatRelationAction::Blockers => GameAction::DeclareBlockers {
            assignments: decoded
                .iter()
                .map(|(source_index, target_index, _)| {
                    let CombatRelationTarget::Object(attacker) = projection.targets[*target_index]
                    else {
                        return Err(InteractionReasonCode::MalformedResponse);
                    };
                    Ok((projection.sources[*source_index], attacker))
                })
                .collect::<Result<_, _>>()?,
        },
    };
    Ok((
        action,
        InteractionProgress {
            selected: relations.len().min(u32::MAX as usize) as u32,
            minimum: 0,
            maximum: Some(projection.max.min(u32::MAX as usize) as u32),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn materialize_mana_group_response(
    interaction_id: &InteractionId,
    projection: &ManaGroupProjection,
    waiting_for: &WaitingFor,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::ManaGroups { choice_ids, count } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    if *count == 0 || *count > projection.max_batch {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let escape_id = interaction_choice_id(interaction_id, 'e', 0);
    if projection.allow_cancel && choice_ids.as_slice() == [escape_id] {
        if *count != 1 {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        return Ok((GameAction::CancelCast, InteractionProgress::default()));
    }
    if choice_ids.len() != projection.groups.len() {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let candidate_indices = (0..projection.candidates.len())
        .map(|index| (interaction_choice_id(interaction_id, 'm', index), index))
        .collect::<HashMap<_, _>>();
    let mut values = vec![None; projection.groups.len()];
    for choice_id in choice_ids {
        let candidate = &projection.candidates[*candidate_indices
            .get(choice_id)
            .ok_or(InteractionReasonCode::UnknownChoice)?];
        if values[candidate.group].replace(candidate.value).is_some() {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
    }
    let values = values
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or(InteractionReasonCode::ConstraintUnsatisfied)?;
    let action = match projection.action {
        ManaGroupAction::PayManaAbility => {
            let payment = values
                .iter()
                .map(|value| match value {
                    ManaGroupCandidateValue::Mana(mana_type) => Ok(*mana_type),
                    ManaGroupCandidateValue::Phyrexian { .. } => {
                        Err(InteractionReasonCode::MalformedResponse)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let WaitingFor::PayManaAbilityMana { options, .. } = waiting_for else {
                return Err(InteractionReasonCode::InvalidAuthorityState);
            };
            if !options.contains(&payment) || *count != 1 {
                return Err(InteractionReasonCode::ConstraintUnsatisfied);
            }
            GameAction::PayManaAbilityMana { payment }
        }
        ManaGroupAction::ChooseSingleColor => {
            let [ManaGroupCandidateValue::Mana(mana_type)] = values.as_slice() else {
                return Err(InteractionReasonCode::MalformedResponse);
            };
            let WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } = waiting_for
            else {
                return Err(InteractionReasonCode::InvalidAuthorityState);
            };
            if !options.contains(mana_type) {
                return Err(InteractionReasonCode::ConstraintUnsatisfied);
            }
            GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(*mana_type),
                count: *count,
            }
        }
        ManaGroupAction::ChooseCombination | ManaGroupAction::ChooseAnyCombination => {
            let combination = values
                .iter()
                .map(|value| match value {
                    ManaGroupCandidateValue::Mana(mana_type) => Ok(*mana_type),
                    ManaGroupCandidateValue::Phyrexian { .. } => {
                        Err(InteractionReasonCode::MalformedResponse)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            if *count != 1 {
                return Err(InteractionReasonCode::ConstraintUnsatisfied);
            }
            match waiting_for {
                WaitingFor::ChooseManaColor {
                    choice: ManaChoicePrompt::Combination { options },
                    ..
                } if projection.action == ManaGroupAction::ChooseCombination => {
                    if !options.contains(&combination) {
                        return Err(InteractionReasonCode::ConstraintUnsatisfied);
                    }
                }
                WaitingFor::ChooseManaColor {
                    choice: ManaChoicePrompt::AnyCombination { count, options },
                    ..
                } if projection.action == ManaGroupAction::ChooseAnyCombination => {
                    if combination.len() != *count
                        || combination.iter().any(|mana| !options.contains(mana))
                    {
                        return Err(InteractionReasonCode::ConstraintUnsatisfied);
                    }
                }
                _ => return Err(InteractionReasonCode::InvalidAuthorityState),
            }
            GameAction::ChooseManaColor {
                choice: ManaChoice::Combination(combination),
                count: 1,
            }
        }
        ManaGroupAction::Phyrexian => {
            if *count != 1 {
                return Err(InteractionReasonCode::ConstraintUnsatisfied);
            }
            let choices = values
                .iter()
                .map(|value| match value {
                    ManaGroupCandidateValue::Phyrexian { choice, .. } => Ok(*choice),
                    ManaGroupCandidateValue::Mana(_) => {
                        Err(InteractionReasonCode::MalformedResponse)
                    }
                })
                .collect::<Result<_, _>>()?;
            GameAction::SubmitPhyrexianChoices { choices }
        }
    };
    let selected = choice_ids.len().min(u32::MAX as usize) as u32;
    let required = projection.groups.iter().map(|group| group.min).sum();
    Ok((
        action,
        InteractionProgress {
            selected,
            minimum: required,
            maximum: Some(required),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn materialize_mode_sequence_response(
    interaction_id: &InteractionId,
    projection: &ModeSequenceProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::Sequence { choice_ids } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    let escape_id = interaction_choice_id(interaction_id, 'e', 0);
    if projection.allow_cancel && choice_ids.as_slice() == [escape_id] {
        return Ok((GameAction::CancelCast, InteractionProgress::default()));
    }
    if choice_ids.len() < projection.min || choice_ids.len() > projection.max {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let candidate_indices = (0..projection.indices.len())
        .map(|index| (interaction_choice_id(interaction_id, 'o', index), index))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::with_capacity(choice_ids.len());
    let indices = choice_ids
        .iter()
        .map(|choice_id| {
            let candidate_index = *candidate_indices
                .get(choice_id)
                .ok_or(InteractionReasonCode::UnknownChoice)?;
            if projection.unique && !seen.insert(candidate_index) {
                return Err(InteractionReasonCode::ConstraintUnsatisfied);
            }
            Ok(projection.indices[candidate_index])
        })
        .collect::<Result<_, _>>()?;
    Ok((
        GameAction::SelectModes { indices },
        InteractionProgress {
            selected: choice_ids.len().min(u32::MAX as usize) as u32,
            minimum: projection.min.min(u32::MAX as usize) as u32,
            maximum: Some(projection.max.min(u32::MAX as usize) as u32),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn materialize_outside_selection_response(
    interaction_id: &InteractionId,
    projection: &OutsideSelectionProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::Select { choice_ids } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    if choice_ids.len() < projection.min || choice_ids.len() > projection.max {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let candidate_indices = (0..projection.candidates.len())
        .map(|index| (interaction_choice_id(interaction_id, 'w', index), index))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::with_capacity(choice_ids.len());
    let selections = choice_ids
        .iter()
        .map(|choice_id| {
            let index = *candidate_indices
                .get(choice_id)
                .ok_or(InteractionReasonCode::UnknownChoice)?;
            if !seen.insert(index) {
                return Err(InteractionReasonCode::ConstraintUnsatisfied);
            }
            Ok(projection.candidates[index].selection.clone())
        })
        .collect::<Result<_, _>>()?;
    Ok((
        GameAction::ChooseOutsideGameCards { selections },
        InteractionProgress {
            selected: choice_ids.len().min(u32::MAX as usize) as u32,
            minimum: projection.min.min(u32::MAX as usize) as u32,
            maximum: Some(projection.max.min(u32::MAX as usize) as u32),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn materialize_text_choice_response(
    projection: &TextChoiceProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::Text { value } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    if !projection.allow_arbitrary && !projection.options.contains(value) {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    Ok((
        GameAction::ChooseOption {
            choice: value.clone(),
        },
        InteractionProgress {
            selected: 1,
            minimum: 1,
            maximum: Some(1),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn materialize_shortcut_reply_response(
    projection: ShortcutReplyProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::ShortcutReply { reply } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    let response = match reply {
        InteractionShortcutReply::Accept => crate::analysis::loop_check::ShortcutResponse::Accept,
        InteractionShortcutReply::Shorten { at_iteration }
            if *at_iteration >= projection.min_iteration
                && *at_iteration <= projection.max_iteration =>
        {
            crate::analysis::loop_check::ShortcutResponse::Shorten {
                at_iteration: *at_iteration,
            }
        }
        InteractionShortcutReply::Shorten { .. } => {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
    };
    Ok((
        GameAction::RespondToShortcut { response },
        InteractionProgress {
            selected: 1,
            minimum: 1,
            maximum: Some(1),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn materialize_direct_choice_response(
    interaction_id: &InteractionId,
    projection: &DirectChoiceProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::Choose { choice_id } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    let action = projection
        .actions
        .iter()
        .enumerate()
        .find(|(index, _)| interaction_choice_id(interaction_id, 'p', *index) == *choice_id)
        .map(|(_, action)| action.clone())
        .ok_or(InteractionReasonCode::UnknownChoice)?;
    Ok((
        action,
        InteractionProgress {
            selected: 1,
            minimum: 1,
            maximum: Some(1),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn materialize_sideboard_response(
    interaction_id: &InteractionId,
    projection: &SideboardProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::DeckPartition { main: assignments } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    if assignments.len() > projection.cards.len() {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let candidate_indices = (0..projection.cards.len())
        .map(|index| (interaction_choice_id(interaction_id, 'b', index), index))
        .collect::<HashMap<_, _>>();
    let mut main_counts = vec![0u32; projection.cards.len()];
    let mut seen = HashSet::with_capacity(assignments.len());
    for assignment in assignments {
        let index = *candidate_indices
            .get(&assignment.choice_id)
            .ok_or(InteractionReasonCode::UnknownChoice)?;
        if !seen.insert(index) || assignment.amount > projection.cards[index].total {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        main_counts[index] = assignment.amount;
    }
    let main_total = main_counts
        .iter()
        .try_fold(0u32, |total, count| total.checked_add(*count));
    if main_total != Some(projection.main_total) {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let main = projection
        .cards
        .iter()
        .zip(&main_counts)
        .filter(|(_, count)| **count > 0)
        .map(|(card, count)| DeckCardCount {
            name: card.name.clone(),
            count: *count,
        })
        .collect();
    let sideboard = projection
        .cards
        .iter()
        .zip(&main_counts)
        .filter_map(|(card, main_count)| {
            let count = card.total - *main_count;
            (count > 0).then(|| DeckCardCount {
                name: card.name.clone(),
                count,
            })
        })
        .collect();
    Ok((
        GameAction::SubmitSideboard { main, sideboard },
        InteractionProgress {
            selected: assignments.len().min(u32::MAX as usize) as u32,
            minimum: projection.main_total,
            maximum: Some(projection.main_total),
            aggregate: i32::try_from(projection.main_total).ok(),
            confirmable: true,
        },
    ))
}

fn materialize_number_response(
    projection: NumberProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::Number { value } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    if *value < projection.min || *value > projection.max {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let action = match projection.action {
        NumberResponseAction::ChooseX => GameAction::ChooseX { value: *value },
        NumberResponseAction::PayAmount => GameAction::SubmitPayAmount { amount: *value },
        NumberResponseAction::AssistPayment => GameAction::CommitAssistPayment { generic: *value },
    };
    Ok((
        action,
        InteractionProgress {
            selected: 1,
            minimum: projection.min,
            maximum: Some(projection.max),
            aggregate: i32::try_from(*value).ok(),
            confirmable: true,
        },
    ))
}

fn materialize_loop_shortcut_response(
    interaction_id: &InteractionId,
    projection: &LoopShortcutProjection,
    proposer: PlayerId,
    authoritative_schema: &crate::analysis::decision_template::ShortcutDecisionSchema,
    authoritative_state: &GameState,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::Shortcut { decision, pins } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    if matches!(decision, InteractionShortcutDecision::Decline) {
        if !pins.is_empty() {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        return Ok((
            GameAction::DeclineShortcut,
            InteractionProgress {
                selected: 1,
                minimum: 1,
                maximum: Some(1),
                aggregate: None,
                confirmable: true,
            },
        ));
    }
    let count = match (*decision, projection.count) {
        (
            InteractionShortcutDecision::AcceptSuggested,
            InteractionShortcutCountSpec::UntilLethal,
        ) => IterationCount::UntilLethal,
        (
            InteractionShortcutDecision::AcceptSuggested,
            InteractionShortcutCountSpec::Fixed { suggested, .. },
        ) => IterationCount::Fixed(suggested),
        (
            InteractionShortcutDecision::Fixed { iterations },
            InteractionShortcutCountSpec::Fixed { min, max, .. },
        ) if iterations >= min && iterations <= max => IterationCount::Fixed(iterations),
        (InteractionShortcutDecision::Fixed { .. }, _) => {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        (InteractionShortcutDecision::Decline, _) => unreachable!("decline returned above"),
    };

    let mut submitted = HashMap::with_capacity(pins.len());
    for pin in pins {
        if submitted.insert(pin.group, pin).is_some() {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
    }
    let mut decisions = Vec::with_capacity(projection.points.len());
    let mut selected = 1u32;
    for (group, point) in projection.points.iter().enumerate() {
        let group = group as u32;
        if point.read_only {
            if submitted.remove(&group).is_some() {
                return Err(InteractionReasonCode::ConstraintUnsatisfied);
            }
            match point.kind {
                InteractionShortcutPointKind::ConvokeTaps => {
                    decisions.push(PinnedDecision::ConvokeTaps {
                        slot: point.slot.clone(),
                    });
                }
                InteractionShortcutPointKind::ManaColor => {
                    let [candidate_index] = point.candidate_indices.as_slice() else {
                        return Err(InteractionReasonCode::InvalidAuthorityState);
                    };
                    let LoopShortcutCandidateValue::ManaColor(color) =
                        &projection.candidates[*candidate_index]
                    else {
                        return Err(InteractionReasonCode::InvalidAuthorityState);
                    };
                    decisions.push(PinnedDecision::ManaColor {
                        slot: point.slot.clone(),
                        color: *color,
                    });
                }
                InteractionShortcutPointKind::Targets
                | InteractionShortcutPointKind::Mode
                | InteractionShortcutPointKind::MayChoice
                | InteractionShortcutPointKind::UnlessBreak => {
                    return Err(InteractionReasonCode::InvalidAuthorityState);
                }
            }
            continue;
        }

        let pin = submitted
            .remove(&group)
            .ok_or(InteractionReasonCode::ConstraintUnsatisfied)?;
        if pin.choice_ids.len() < point.min as usize
            || pin.choice_ids.len() > point.max as usize
            || (point.unique
                && pin.choice_ids.iter().collect::<HashSet<_>>().len() != pin.choice_ids.len())
        {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        selected = selected
            .checked_add(pin.choice_ids.len() as u32)
            .ok_or(InteractionReasonCode::PayloadTooLarge)?;
        let candidate_indices = pin
            .choice_ids
            .iter()
            .map(|choice_id| {
                point
                    .candidate_indices
                    .iter()
                    .copied()
                    .find(|index| interaction_choice_id(interaction_id, 'k', *index) == *choice_id)
                    .ok_or(InteractionReasonCode::UnknownChoice)
            })
            .collect::<Result<Vec<_>, _>>()?;
        match point.kind {
            InteractionShortcutPointKind::Targets => {
                let targets = candidate_indices
                    .iter()
                    .map(|index| match &projection.candidates[*index] {
                        LoopShortcutCandidateValue::Target(TargetRef::Player(player)) => {
                            Ok(TargetPin::Player(*player))
                        }
                        LoopShortcutCandidateValue::Target(TargetRef::Object(object_id)) => {
                            let object = authoritative_state
                                .objects
                                .get(object_id)
                                .ok_or(InteractionReasonCode::ConstraintUnsatisfied)?;
                            // CR 400.7: bind the submitted target to this object's current
                            // incarnation so a zone change cannot silently retarget the replay.
                            Ok(TargetPin::ByIdentity(
                                crate::types::game_state::YieldTarget::ThisObject {
                                    source_id: *object_id,
                                    incarnation: Some(object.incarnation),
                                    trigger_description: None,
                                },
                            ))
                        }
                        _ => Err(InteractionReasonCode::InvalidAuthorityState),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                decisions.push(PinnedDecision::Targets {
                    slot: point.slot.clone(),
                    targets,
                });
            }
            InteractionShortcutPointKind::Mode => {
                let indices = candidate_indices
                    .iter()
                    .map(|index| match &projection.candidates[*index] {
                        LoopShortcutCandidateValue::Mode(mode) => Ok(*mode),
                        _ => Err(InteractionReasonCode::InvalidAuthorityState),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                decisions.push(PinnedDecision::Mode {
                    slot: point.slot.clone(),
                    indices,
                });
            }
            InteractionShortcutPointKind::MayChoice => {
                let [candidate_index] = candidate_indices.as_slice() else {
                    return Err(InteractionReasonCode::ConstraintUnsatisfied);
                };
                let LoopShortcutCandidateValue::May(take) =
                    &projection.candidates[*candidate_index]
                else {
                    return Err(InteractionReasonCode::InvalidAuthorityState);
                };
                decisions.push(PinnedDecision::MayChoice {
                    slot: point.slot.clone(),
                    take: *take,
                });
            }
            InteractionShortcutPointKind::UnlessBreak => {
                let [candidate_index] = candidate_indices.as_slice() else {
                    return Err(InteractionReasonCode::ConstraintUnsatisfied);
                };
                let LoopShortcutCandidateValue::Unless(pay) =
                    &projection.candidates[*candidate_index]
                else {
                    return Err(InteractionReasonCode::InvalidAuthorityState);
                };
                decisions.push(PinnedDecision::UnlessBreak {
                    slot: point.slot.clone(),
                    pay: *pay,
                });
            }
            InteractionShortcutPointKind::ConvokeTaps | InteractionShortcutPointKind::ManaColor => {
                return Err(InteractionReasonCode::InvalidAuthorityState);
            }
        }
    }
    if !submitted.is_empty() {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }

    let sources = projection
        .points
        .iter()
        .map(|point| point.slot.source.clone())
        .collect::<Vec<_>>();
    let template = (!projection.points.is_empty()).then(|| DecisionTemplate {
        owner: proposer,
        decisions,
        replay: ReplayMode::Scheduled {
            count: count.clone(),
        },
        key: DecisionGroupKey::from_sources(&sources, DecisionKind::LoopChoice),
    });
    if let Some(template) = &template {
        let required = authoritative_schema
            .points
            .iter()
            .map(|point| point.slot.clone())
            .collect::<Vec<_>>();
        if predictability_gate(template, &required).is_err()
            || validate_pins(authoritative_schema, template, 1, authoritative_state).is_err()
        {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
    }
    let pin_minimum = projection
        .points
        .iter()
        .filter(|point| !point.read_only)
        .map(|point| point.min)
        .sum::<u32>();
    let pin_maximum = projection
        .points
        .iter()
        .filter(|point| !point.read_only)
        .map(|point| point.max)
        .sum::<u32>();
    Ok((
        GameAction::DeclareShortcut { count, template },
        InteractionProgress {
            selected,
            minimum: 1 + pin_minimum,
            maximum: Some(1 + pin_maximum),
            aggregate: None,
            confirmable: true,
        },
    ))
}

fn decode_amount_assignments(
    interaction_id: &InteractionId,
    candidates: &[AssignmentCandidate],
    assignments: &[AmountAssignment],
) -> Result<Vec<(usize, u32)>, InteractionReasonCode> {
    let mut seen = HashSet::with_capacity(assignments.len());
    let mut decoded = Vec::with_capacity(assignments.len());
    for assignment in assignments {
        if assignment.amount == 0 {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        let index = (0..candidates.len())
            .find(|index| {
                interaction_choice_id(interaction_id, 'a', *index) == assignment.choice_id
            })
            .ok_or(InteractionReasonCode::UnknownChoice)?;
        if !seen.insert(index) || assignment.amount > candidates[index].available {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        decoded.push((index, assignment.amount));
    }
    Ok(decoded)
}

fn decoded_total(decoded: &[(usize, u32)]) -> Result<u32, InteractionReasonCode> {
    decoded.iter().try_fold(0u32, |total, (_, amount)| {
        total
            .checked_add(*amount)
            .ok_or(InteractionReasonCode::PayloadTooLarge)
    })
}

fn materialize_amount_assignment_response(
    interaction_id: &InteractionId,
    projection: &AmountAssignmentProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::AssignAmounts { assignments } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    let decoded = decode_amount_assignments(interaction_id, &projection.candidates, assignments)?;
    if projection.require_all && decoded.len() != projection.candidates.len() {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let total = decoded_total(&decoded)?;
    if total < projection.min_total
        || total > projection.max_total
        || projection.exact_total.is_some_and(|exact| total != exact)
    {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let action = match projection.action {
        AmountAssignmentAction::BlockerDamage => GameAction::AssignBlockerDamage {
            assignments: decoded
                .iter()
                .map(|(index, amount)| match projection.candidates[*index].kind {
                    AssignmentCandidateKind::Object(object_id) => Ok((object_id, *amount)),
                    _ => Err(InteractionReasonCode::MalformedResponse),
                })
                .collect::<Result<_, _>>()?,
        },
        AmountAssignmentAction::DistributeAmong => GameAction::DistributeAmong {
            distribution: decoded
                .iter()
                .map(
                    |(index, amount)| match &projection.candidates[*index].kind {
                        AssignmentCandidateKind::Target(target) => Ok((target.clone(), *amount)),
                        _ => Err(InteractionReasonCode::MalformedResponse),
                    },
                )
                .collect::<Result<_, _>>()?,
        },
        AmountAssignmentAction::MoveCounters => GameAction::ChooseCounterMoveDistribution {
            selections: decoded
                .iter()
                .map(
                    |(index, amount)| match &projection.candidates[*index].kind {
                        AssignmentCandidateKind::CounterMove {
                            destination_id,
                            counter_type,
                        } => Ok(CounterMoveChoice {
                            destination_id: *destination_id,
                            counter_type: counter_type.clone(),
                            count: *amount,
                        }),
                        _ => Err(InteractionReasonCode::MalformedResponse),
                    },
                )
                .collect::<Result<_, _>>()?,
        },
        AmountAssignmentAction::RemoveCounters => GameAction::ChooseCountersToRemove {
            selections: decoded
                .iter()
                .map(
                    |(index, amount)| match &projection.candidates[*index].kind {
                        AssignmentCandidateKind::CounterRemove { counter_type } => {
                            Ok(CounterRemoveChoice {
                                counter_type: counter_type.clone(),
                                count: *amount,
                            })
                        }
                        _ => Err(InteractionReasonCode::MalformedResponse),
                    },
                )
                .collect::<Result<_, _>>()?,
        },
    };
    Ok((
        action,
        InteractionProgress {
            selected: decoded.len().min(u32::MAX as usize) as u32,
            minimum: projection.min_total,
            maximum: Some(projection.max_total),
            aggregate: i32::try_from(total).ok(),
            confirmable: true,
        },
    ))
}

fn materialize_damage_assignment_response(
    interaction_id: &InteractionId,
    projection: &DamageAssignmentProjection,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let InteractionResponse::AssignDamage { mode, assignments } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    if !projection.modes.contains(mode) {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    if *mode == InteractionDamageAssignmentMode::AsThoughUnblocked {
        if !assignments.is_empty() {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        return Ok((
            GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::AsThoughUnblocked,
                assignments: Vec::new(),
                trample_damage: 0,
                controller_damage: 0,
            },
            InteractionProgress {
                selected: 0,
                minimum: 0,
                maximum: Some(0),
                aggregate: Some(0),
                confirmable: true,
            },
        ));
    }
    let decoded = decode_amount_assignments(interaction_id, &projection.candidates, assignments)?;
    let total = decoded_total(&decoded)?;
    if total != projection.total {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let mut blocker_assignments = Vec::new();
    let mut trample_damage = 0;
    let mut controller_damage = 0;
    for (index, amount) in decoded {
        if index < projection.blocker_count {
            let AssignmentCandidateKind::Object(object_id) = projection.candidates[index].kind
            else {
                return Err(InteractionReasonCode::MalformedResponse);
            };
            blocker_assignments.push((object_id, amount));
        } else if projection.has_trample_target && index == projection.blocker_count {
            trample_damage = amount;
        } else if projection.has_controller_target {
            controller_damage = amount;
        } else {
            return Err(InteractionReasonCode::MalformedResponse);
        }
    }
    Ok((
        GameAction::AssignCombatDamage {
            mode: CombatDamageAssignmentMode::Normal,
            assignments: blocker_assignments,
            trample_damage,
            controller_damage,
        },
        InteractionProgress {
            selected: assignments.len().min(u32::MAX as usize) as u32,
            minimum: projection.total,
            maximum: Some(projection.total),
            aggregate: i32::try_from(total).ok(),
            confirmable: true,
        },
    ))
}

fn materialize_response(
    authoritative_state: &GameState,
    filtered_state: &GameState,
    interaction_id: &InteractionId,
    response: &InteractionResponse,
) -> Result<(GameAction, InteractionProgress), InteractionReasonCode> {
    let semantic_owner = authoritative_state
        .active_interaction_slots
        .iter()
        .find(|slot| slot.interaction_id == *interaction_id)
        .map(|slot| PlayerId(slot.semantic_owner))
        .ok_or(InteractionReasonCode::StaleInteraction)?;
    match human_response_model(&filtered_state.waiting_for, semantic_owner) {
        HumanResponseModel::Terminal => return Err(InteractionReasonCode::UnsupportedResponse),
        HumanResponseModel::TriggerOrder => {
            let projection = trigger_order_projection(&filtered_state.waiting_for)?
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_trigger_order_response(interaction_id, &projection, response);
        }
        HumanResponseModel::CoinFlipSequence => {
            let projection = coin_flip_projection(&filtered_state.waiting_for)?
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_coin_flip_response(interaction_id, projection, response);
        }
        HumanResponseModel::TargetSequence => {
            let projection = target_sequence_projection(&filtered_state.waiting_for)?
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_target_sequence_response(interaction_id, &projection, response);
        }
        HumanResponseModel::CategorySelection => {
            let projection = category_selection_projection(&filtered_state.waiting_for)?
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_category_selection_response(interaction_id, &projection, response);
        }
        HumanResponseModel::CombatRelations(expected_action) => {
            let projection =
                combat_relation_projection(&filtered_state.waiting_for, expected_action)?
                    .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_combat_relation_response(interaction_id, &projection, response);
        }
        HumanResponseModel::ManaGroups(expected_action) => {
            let projection = mana_group_projection(&filtered_state.waiting_for, expected_action)?
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_mana_group_response(
                interaction_id,
                &projection,
                &filtered_state.waiting_for,
                response,
            );
        }
        HumanResponseModel::ModeSequence => {
            let projection = mode_sequence_projection(&filtered_state.waiting_for)?
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_mode_sequence_response(interaction_id, &projection, response);
        }
        HumanResponseModel::OutsideSelection => {
            let projection = outside_selection_projection(&filtered_state.waiting_for)?
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_outside_selection_response(interaction_id, &projection, response);
        }
        HumanResponseModel::TextChoice => {
            let projection = text_choice_projection(&filtered_state.waiting_for)?
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_text_choice_response(&projection, response);
        }
        HumanResponseModel::ShortcutReply => {
            let projection = shortcut_reply_projection(&filtered_state.waiting_for)
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_shortcut_reply_response(projection, response);
        }
        HumanResponseModel::DirectChoices => {
            let projection = direct_choice_projection(
                &filtered_state.waiting_for,
                filtered_state,
                semantic_owner,
            )?
            .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_direct_choice_response(interaction_id, &projection, response);
        }
        HumanResponseModel::SideboardPartition => {
            let projection =
                sideboard_projection(&filtered_state.waiting_for, filtered_state, semantic_owner)?
                    .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_sideboard_response(interaction_id, &projection, response);
        }
        HumanResponseModel::NumberRange(expected_action) => {
            let projection = number_projection(&filtered_state.waiting_for)
                .filter(|projection| projection.action == expected_action)
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_number_response(projection, response);
        }
        HumanResponseModel::LoopShortcut => {
            let projection = loop_shortcut_projection(&filtered_state.waiting_for)?;
            let WaitingFor::LoopShortcut {
                proposer, schema, ..
            } = &authoritative_state.waiting_for
            else {
                return Err(InteractionReasonCode::InvalidAuthorityState);
            };
            if *proposer != semantic_owner {
                return Err(InteractionReasonCode::InvalidAuthorityState);
            }
            return materialize_loop_shortcut_response(
                interaction_id,
                &projection,
                *proposer,
                schema,
                authoritative_state,
                response,
            );
        }
        HumanResponseModel::AmountAssignments => {
            let projection = amount_assignment_projection(&filtered_state.waiting_for)?
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_amount_assignment_response(interaction_id, &projection, response);
        }
        HumanResponseModel::DamageAssignments => {
            let projection = damage_assignment_projection(&filtered_state.waiting_for)?
                .ok_or(InteractionReasonCode::UnsupportedResponse)?;
            return materialize_damage_assignment_response(interaction_id, &projection, response);
        }
        HumanResponseModel::ExactCandidates(AuditedExactCandidates)
        | HumanResponseModel::Select
        | HumanResponseModel::AssignAmounts => {}
    }
    if let Some(projection) =
        counter_distribution_projection(&filtered_state.waiting_for, filtered_state)?
    {
        return materialize_counter_response(interaction_id, &projection, response);
    }
    if let Some(selection) =
        selection_projection(&filtered_state.waiting_for, filtered_state, semantic_owner)?
    {
        let InteractionResponse::Select { choice_ids } = response else {
            return Err(InteractionReasonCode::MalformedResponse);
        };
        let selected = selected_objects_from_ids(interaction_id, &selection, choice_ids)?;
        let progress = selection_progress(
            &selection,
            &selected,
            &filtered_state.waiting_for,
            filtered_state,
        );
        if !progress.confirmable {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        return Ok((selection_action(&selection, selected)?, progress));
    }

    let InteractionResponse::Choose { choice_id } = response else {
        return Err(InteractionReasonCode::MalformedResponse);
    };
    let candidates = actor_candidates(authoritative_state, semantic_owner)?;
    let action = candidates
        .iter()
        .enumerate()
        .find(|(index, _)| interaction_choice_id(interaction_id, 'c', *index) == *choice_id)
        .map(|(_, candidate)| candidate.action.clone())
        .ok_or(InteractionReasonCode::UnknownChoice)?;
    Ok((action, InteractionProgress::default()))
}

fn preview_outcome(
    before: &GameState,
    after: &GameState,
    interaction_id: &InteractionId,
) -> InteractionOutcomeCode {
    if after
        .active_interaction_slots
        .iter()
        .any(|slot| slot.interaction_id == *interaction_id)
    {
        InteractionOutcomeCode::Preserved
    } else if matches!(after.waiting_for, WaitingFor::GameOver { .. }) {
        InteractionOutcomeCode::Terminal
    } else if after.active_interaction_slots.is_empty() {
        InteractionOutcomeCode::Cleared
    } else if std::mem::discriminant(&before.waiting_for)
        == std::mem::discriminant(&after.waiting_for)
    {
        InteractionOutcomeCode::Replaced
    } else {
        InteractionOutcomeCode::Advanced
    }
}

/// Preview a response by materializing the exact engine action and applying it
/// to a throwaway authoritative clone. No global-concede action is ever minted
/// by the interaction candidate set, so it can never serve as a progress witness.
pub fn preview_interaction(
    state: &GameState,
    actor: PlayerId,
    request: &InteractionPreviewRequest,
) -> InteractionPreview {
    let rejected = |reason, progress| InteractionPreview {
        request_id: request.request_id.clone(),
        interaction_id: request.interaction_id.clone(),
        status: InteractionPreviewStatus::Rejected { reason },
        progress,
        outcome: InteractionOutcomeCode::Rejected,
        summaries: vec![InteractionSummaryCode::ConfirmUnavailable],
    };

    if bound_string(request.request_id.as_str())
        .and_then(|_| bound_string(request.interaction_id.as_str()))
        .and_then(|_| validate_response_bounds(&request.response))
        .is_err()
    {
        return rejected(
            InteractionReasonCode::PayloadTooLarge,
            InteractionProgress::default(),
        );
    }
    let semantic_owner = match slot_for_submission(state, actor, &request.interaction_id) {
        Ok(slot) => PlayerId(slot.semantic_owner),
        Err(reason) => return rejected(reason, InteractionProgress::default()),
    };
    let filtered = visibility::filter_state_for_viewer(state, actor);
    let (action, progress) =
        match materialize_response(state, &filtered, &request.interaction_id, &request.response) {
            Ok(materialized) => materialized,
            Err(reason) => return rejected(reason, InteractionProgress::default()),
        };
    let mut projected = state.clone();
    match apply_interaction_for_simulation(&mut projected, actor, semantic_owner, action) {
        Ok(_) => InteractionPreview {
            request_id: request.request_id.clone(),
            interaction_id: request.interaction_id.clone(),
            status: InteractionPreviewStatus::Confirmable,
            progress: InteractionProgress {
                confirmable: true,
                ..progress
            },
            outcome: preview_outcome(state, &projected, &request.interaction_id),
            summaries: vec![
                InteractionSummaryCode::ConfirmAvailable,
                InteractionSummaryCode::Progress,
            ],
        },
        Err(_) => rejected(InteractionReasonCode::ReducerRejected, progress),
    }
}

/// Hidden engine-only submission entry point. The opaque interaction and choice
/// IDs are looked up against current trusted state, authorization is rechecked,
/// projection is recomputed from a viewer-filtered clone, and the materialized
/// action enters the same actor guard/reducer/outward boundary as legacy actions.
pub fn submit_interaction(
    state: &mut GameState,
    actor: PlayerId,
    submission: InteractionSubmission,
) -> Result<ActionResult, InteractionSubmitError> {
    bound_string(submission.interaction_id.as_str())?;
    validate_response_bounds(&submission.response)?;
    let semantic_owner =
        PlayerId(slot_for_submission(state, actor, &submission.interaction_id)?.semantic_owner);
    let filtered = visibility::filter_state_for_viewer(state, actor);
    let (action, _) = materialize_response(
        state,
        &filtered,
        &submission.interaction_id,
        &submission.response,
    )?;
    apply_interaction(state, actor, semantic_owner, action).map_err(|_error: EngineError| {
        InteractionSubmitError {
            code: InteractionReasonCode::ReducerRejected,
        }
    })
}
