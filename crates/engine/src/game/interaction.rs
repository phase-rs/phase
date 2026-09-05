//! Hidden engine-authority interaction projection and submission boundary.
//!
//! Production adapters consume this projection while the existing action UI
//! remains the exposed authority until its separately reviewed cutover.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ai_support::{
    validated_candidate_actions_for_semantic_owner, ActionMetadata, CandidateAction,
    FilterPipeline, TacticalClass,
};
use crate::analysis::decision_template::{
    declaration_conforms, AnnouncementSubject, DecisionGroupKey, DecisionKind, DecisionTemplate,
    IterationCount, PinnedDecision, Ranking, ReplayMode, TargetPin, TargetSchedule,
};
use crate::types::ability::{
    AggregateFunction, ChoiceType, ChooseFromZoneConstraint, Comparator, CounterCostSelection,
    DoorLockOp, EffectKind, ObjectProperty, SearchSelectionConstraint, TapCreaturesAggregateStat,
    TapCreaturesSelectionMode, TargetRef,
};
use crate::types::action_rejection::{ActionRejection, ActionRejectionCode};
use crate::types::actions::{
    AlternativeCastDecision, CastChoice, GameAction, MulliganChoice, OutsideGameSelection,
    PrecastCopyShortcutResponse, ResolutionOptionalPaymentChoice, UnlessCostBranch,
};
use crate::types::card_type::CoreType;
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::game_state::{
    ActionResult, AutoMayChoice, CastPaymentMode, CastingVariant, CombatDamageAssignmentMode,
    ConvokeMode, CounterCostChoice, CounterMoveChoice, CounterRemoveChoice, GameState, ManaChoice,
    ManaChoiceContext, ManaChoicePrompt, MayTriggerAutoChoiceScope, OutsideGameChoiceSource,
    PayCostKind, PileSide, PtDirection, ShardChoice, ShardOptions, TargetEffectDetail, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::interaction::{
    ActiveInteractionSlot, AggregateComparator, AmountAssignment, ConfirmSemantics,
    InteractionActionCode, InteractionActionId, InteractionAggregateFunction,
    InteractionAttachmentFan, InteractionAttachmentFanChild, InteractionAttachmentView,
    InteractionAttachmentViewCard, InteractionAvailability, InteractionChoice, InteractionChoiceId,
    InteractionChoiceStatus, InteractionDamageAssignmentMode, InteractionGroupConstraint,
    InteractionId, InteractionIntentCode, InteractionManaAbilityActivationScope,
    InteractionManaColor, InteractionManaComparator, InteractionManaRestriction,
    InteractionManaSpecialAction, InteractionManaSpellCostCriterion,
    InteractionManaZoneSpendPolarity, InteractionObjectProperty, InteractionOpportunity,
    InteractionOpportunityResponse, InteractionOutcomeCode, InteractionPresentationSurface,
    InteractionPreview, InteractionPreviewRequest, InteractionPreviewStatus, InteractionProgress,
    InteractionReasonCode, InteractionRelationConstraint, InteractionRelationSourceConstraint,
    InteractionResponse, InteractionResponseSpec, InteractionRoleCode, InteractionSessionId,
    InteractionShortcutCountSpec, InteractionShortcutDecision, InteractionShortcutPin,
    InteractionShortcutPoint, InteractionShortcutPointKind, InteractionShortcutPreview,
    InteractionShortcutPreviewEntry, InteractionShortcutPreviewFamily, InteractionShortcutReply,
    InteractionShortcutResponseCode, InteractionSlotKind, InteractionSubmission,
    InteractionSummaryCode, InteractionWaitingForCode, InteractionWaitingForKind,
    InteractionZoneCode, SelectionConstraint, SimultaneousDecisionKind, ViewerInteraction,
    MAX_INTERACTION_LIST_LEN, MAX_SHORTCUT_PREVIEW_ELEMENTS,
};
use crate::types::mana::{
    AbilityActivationScope, ManaColor, ManaCost, ManaRestriction, ManaSourceSelection, ManaType,
    SpecialAction, SpellCostCriterion, ZoneSpendPolarity,
};
use crate::types::match_config::DeckCardCount;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::combat::AttackTarget;
use super::derived_views::{family_of, payload_seat, UnboundedFamily};
use super::dungeon::DungeonId;
use super::engine::{
    action_rejection_for_engine_error, apply_interaction, apply_interaction_for_simulation,
    apply_interaction_with_rejection, EngineError, MAX_SHORTCUT_CYCLES,
};
use super::game_object::{AttachTarget, RoomDoor};
use super::merge::MergeSide;
use super::{mana_sources, turn_control, visibility};

pub const MAX_INTERACTION_STRING_LEN: usize = 256;
const MAX_INTERACTION_SESSION_ID_LEN: usize = 128;
const MAX_INTERACTION_SERIAL_LEN: usize = 32;

fn action_rejection_for_interaction_reason(reason: InteractionReasonCode) -> ActionRejection {
    let code = match reason {
        InteractionReasonCode::AuthorityUnbound | InteractionReasonCode::InvalidAuthorityState => {
            ActionRejectionCode::InteractionUnavailable
        }
        InteractionReasonCode::NotAuthorized => ActionRejectionCode::InteractionNotAuthorized,
        InteractionReasonCode::StaleInteraction => ActionRejectionCode::StaleInteraction,
        InteractionReasonCode::UnknownChoice | InteractionReasonCode::MalformedResponse => {
            ActionRejectionCode::InvalidInteractionResponse
        }
        InteractionReasonCode::PayloadTooLarge => ActionRejectionCode::InteractionPayloadTooLarge,
        InteractionReasonCode::ConstraintUnsatisfied | InteractionReasonCode::NoLegalResponse => {
            ActionRejectionCode::InteractionConstraintUnsatisfied
        }
        InteractionReasonCode::CancelOnly => ActionRejectionCode::InteractionCancelOnly,
        InteractionReasonCode::ReducerRejected => ActionRejectionCode::InteractionReducerRejected,
        InteractionReasonCode::UnsupportedResponse => {
            ActionRejectionCode::UnsupportedInteractionResponse
        }
    };
    ActionRejection::from_code(code, vec![])
}

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
        // Resolve All consent has a finite, engine-authored Grant/Decline or
        // Revoke domain. It is not a CR 732 shortcut-reply protocol.
        WaitingFor::ResolveAllConsent { .. } | WaitingFor::ResolveAllReady { .. } => {
            HumanResponseModel::ExactCandidates(AuditedExactCandidates)
        }
        WaitingFor::PrecastCopyShortcutOffer { .. }
        | WaitingFor::RespondToPrecastCopyShortcut { .. }
        | WaitingFor::CommanderZoneChoice { .. }
        | WaitingFor::UntapChoice { .. } => HumanResponseModel::DirectChoices,
        WaitingFor::BetweenGamesSideboard { .. } => HumanResponseModel::SideboardPartition,
        WaitingFor::ManaPayment { .. } | WaitingFor::ManaSourceSelection { .. } => {
            HumanResponseModel::DirectChoices
        }
        WaitingFor::LoopShortcut { .. } => HumanResponseModel::LoopShortcut,
        WaitingFor::Priority { .. }
        | WaitingFor::MeldPairChoice { .. }
        | WaitingFor::MeldAttackTargetChoice { .. }
        | WaitingFor::EntryAttackTargetChoice { .. }
        | WaitingFor::MulliganDecision { .. }
        | WaitingFor::AssistChoosePlayer { .. }
        | WaitingFor::ExertChoice { .. }
        | WaitingFor::EnlistChoice { .. }
        | WaitingFor::ReplacementChoice { .. }
        | WaitingFor::EntryControllerChoice { .. }
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
        | WaitingFor::ResolutionOptionalPaymentChoice { .. }
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
        | WaitingFor::ChooseGiftRecipient { .. }
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
        | WaitingFor::ManaSourceSelection { .. }
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
        WaitingFor::ResolveAllConsent { .. } => (
            InteractionWaitingForCode::Shortcut,
            Some(SimultaneousDecisionKind::ResolveAllConsent),
            Some(InteractionSlotKind::Single),
        ),
        WaitingFor::LoopShortcut { .. }
        | WaitingFor::RespondToShortcut { .. }
        | WaitingFor::ResolveAllReady { .. } => (
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
        | WaitingFor::EntryAttackTargetChoice { .. }
        | WaitingFor::ChooseXValue { .. }
        | WaitingFor::UntapChoice { .. }
        | WaitingFor::ExertChoice { .. }
        | WaitingFor::EnlistChoice { .. }
        | WaitingFor::ReplacementChoice { .. }
        | WaitingFor::EntryControllerChoice { .. }
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
        | WaitingFor::ResolutionOptionalPaymentChoice { .. }
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
        | WaitingFor::ChooseGiftRecipient { .. }
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

fn semantic_slots(state: &GameState) -> Vec<(PlayerId, InteractionSlotKind)> {
    let classification = classify_waiting_for(&state.waiting_for);
    let Some(slot_kind) = classification.slot_kind else {
        return Vec::new();
    };
    match &state.waiting_for {
        WaitingFor::ResolveAllConsent {
            epoch,
            representative,
        } => state
            .resolve_all_consent_run
            .as_ref()
            .filter(|run| run.epoch == *epoch)
            .map(|run| {
                run.participants
                    .iter()
                    .filter(|participant| {
                        participant.granted && participant.representative != *representative
                    })
                    .map(|participant| (participant.representative, slot_kind))
                    .chain(std::iter::once((*representative, slot_kind)))
                    .collect()
            })
            .unwrap_or_default(),
        WaitingFor::ResolveAllReady { epoch } => state
            .resolve_all_consent_run
            .as_ref()
            .filter(|run| run.epoch == *epoch)
            .map(|run| {
                run.participants
                    .iter()
                    .filter(|participant| participant.granted)
                    .map(|participant| (participant.representative, slot_kind))
                    .collect()
            })
            .unwrap_or_default(),
        _ => state
            .waiting_for
            .acting_players()
            .into_iter()
            .map(|player| (player, slot_kind))
            .collect(),
    }
}

fn interaction_submitter_for_owner(state: &GameState, semantic_owner: PlayerId) -> PlayerId {
    let frozen = match &state.waiting_for {
        WaitingFor::ResolveAllConsent { epoch, .. } | WaitingFor::ResolveAllReady { epoch } => {
            turn_control::resolve_all_granted_submitter(state, *epoch, semantic_owner)
        }
        _ => None,
    };
    frozen.unwrap_or_else(|| turn_control::authorized_submitter_for_player(state, semantic_owner))
}

fn interaction_authorized_submitters(state: &GameState) -> Vec<PlayerId> {
    let mut submitters = Vec::new();
    for (owner, _) in semantic_slots(state) {
        let submitter = interaction_submitter_for_owner(state, owner);
        if !submitters.contains(&submitter) {
            submitters.push(submitter);
        }
    }
    submitters
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
    let semantic = semantic_slots(state);
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
    let expected = semantic_slots(state);
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
    semantic_slots(state)
        .into_iter()
        .map(|(owner, _)| owner)
        .find(|owner| interaction_submitter_for_owner(state, *owner) == actor)
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
    let next = semantic_slots(state);
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
    #[cfg(not(debug_assertions))]
    let _ = state;

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
        let expected = semantic_slots(state);
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
    /// CR 115.1: what the announcing spell/ability will do to the chosen
    /// target, derived from the current slot's `effect_kind`. Only the two
    /// slot-carrying states (`TargetSelection`, `TriggerTargetSelection`) can
    /// answer this; every other state on this model is a "choose" without a
    /// per-slot effect attribution and stays neutral.
    intent: InteractionIntentCode,
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

/// CR 732.2a: one proposer's declaration, decoded into the vocabulary the OFFER side already
/// projects, so the responder's element is minted by the offer's producers rather than by a
/// second copy of them.
///
/// `segments` is positionally aligned with `projection.points`: the two come out of ONE `unzip`
/// of one accumulator at a single site, so they are built in lockstep. That is
/// correct-by-construction, not a type-level guarantee — this struct can be built with a
/// mismatched pair, which is why `declared_sequence_preview` length-checks the entry it reads.
/// The entry for a point that declares no partition is EMPTY; otherwise it is aligned with that
/// point's own candidates and sums to the declared count, because a piecewise schedule's starts
/// are that partition's own prefix sums. WHICH entry an element is minted from is decided by
/// `allocation_point`'s index, not by this walk's order — see `declared_sequence_preview`.
#[derive(Debug, Clone)]
struct DeclaredSequence {
    projection: LoopShortcutProjection,
    segments: Vec<Vec<u32>>,
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
    /// CR 732.2a: the certificate's measured per-period signature, carried UNMULTIPLIED.
    /// The count-keyed magnitudes are minted downstream by `loop_shortcut_preview`, which
    /// needs the offer's interaction id to speak the published candidate ids — a binding
    /// this projection's own parameter list is pinned against holding.
    per_cycle: Option<crate::analysis::resource::PeriodicDelta>,
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
    /// CR 100.2a / CR 100.4a: inclusive bounds on the main-deck total. The pool
    /// is invariant across the partition, so the minimum deck size and the
    /// sideboard cap both reduce to limits on this one number.
    min_main_total: u32,
    max_main_total: u32,
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
            let slot = target_slots.get(selection.current_slot);
            let optional = slot.is_some_and(|slot| slot.optional);
            TargetSequenceProjection {
                candidates: selection.current_legal_targets.clone(),
                min: usize::from(!optional),
                max: 1,
                unique: true,
                action: TargetSequenceAction::ChooseTarget,
                // CR 601.2c: targets are announced one slot at a time, and each
                // slot carries its own effect, so the label is per-slot rather
                // than per-spell. A chained "destroy target creature. Draw a
                // card. Target player gains 2 life" announces two slots with
                // two different intents.
                intent: slot.map_or(InteractionIntentCode::Choose, |slot| {
                    target_intent(slot.effect_kind, slot.effect_detail)
                }),
            }
        }
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => {
            let slot = target_slots.get(selection.current_slot);
            let optional = slot.is_some_and(|slot| slot.optional);
            TargetSequenceProjection {
                candidates: selection.current_legal_targets.clone(),
                min: usize::from(!optional),
                max: 1,
                unique: true,
                action: TargetSequenceAction::ChooseTarget,
                // CR 601.2c: targets are announced one slot at a time, and each
                // slot carries its own effect, so the label is per-slot rather
                // than per-spell. A chained "destroy target creature. Draw a
                // card. Target player gains 2 life" announces two slots with
                // two different intents.
                intent: slot.map_or(InteractionIntentCode::Choose, |slot| {
                    target_intent(slot.effect_kind, slot.effect_detail)
                }),
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
            intent: InteractionIntentCode::Choose,
        },
        WaitingFor::ChooseObjectsSelection {
            eligible, min, max, ..
        } => TargetSequenceProjection {
            candidates: eligible.clone(),
            min: *min as usize,
            max: max
                .map(|maximum| maximum as usize)
                .unwrap_or(eligible.len())
                .min(eligible.len()),
            unique: true,
            action: TargetSequenceAction::SelectTargets,
            intent: InteractionIntentCode::Choose,
        },
        WaitingFor::EachPlayerCopyChosenSelection {
            eligible, min, max, ..
        } => TargetSequenceProjection {
            candidates: eligible.clone(),
            min: *min as usize,
            max: *max as usize,
            unique: true,
            action: TargetSequenceAction::SelectTargets,
            intent: InteractionIntentCode::Choose,
        },
        WaitingFor::ProliferateChoice { eligible, .. }
        | WaitingFor::TimeTravelChoice { eligible, .. } => TargetSequenceProjection {
            candidates: eligible.clone(),
            min: 0,
            max: eligible.len(),
            unique: true,
            action: TargetSequenceAction::SelectTargets,
            intent: InteractionIntentCode::Choose,
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
                // CR 115.7: retargeting changes an existing spell's targets. The
                // intent belongs to that spell, not to this choice, and this
                // state carries no slot to read it from.
                intent: InteractionIntentCode::Choose,
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
    let has_delve = state.pending_cast.as_ref().is_some_and(|pending| {
        super::casting::spell_has_delve_payment_for(
            state,
            player,
            pending.object_id,
            pending.casting_variant == CastingVariant::Fuse,
        )
    });
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
        Some(ConvokeMode::Delve) => 0,
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
    let delve_upper_bound = if has_delve {
        state
            .objects
            .values()
            .filter(|object| object.is_delve_eligible(player))
            .count()
    } else {
        0
    };
    let total_upper_bound = actions
        .len()
        .checked_add(tapped_for_mana.len())
        .and_then(|count| count.checked_add(pool.mana_pool.mana.len()))
        .and_then(|count| count.checked_add(convoke_upper_bound))
        .and_then(|count| count.checked_add(delve_upper_bound))
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
    if has_delve {
        actions.extend(state.objects.values().filter_map(|object| {
            object
                .is_delve_eligible(player)
                .then_some(GameAction::TapForConvoke {
                    object_id: object.id,
                    mana_type: ManaType::Colorless,
                })
        }));
    }
    match convoke_mode {
        None | Some(ConvokeMode::Delve) => {}
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
        WaitingFor::ManaSourceSelection {
            player, options, ..
        } => {
            if *player != semantic_owner {
                return Err(InteractionReasonCode::InvalidAuthorityState);
            }
            let mut actions = options
                .iter()
                .cloned()
                .map(|selection| GameAction::ActivateManaSource { selection })
                .collect::<Vec<_>>();
            actions.push(GameAction::BackToManaPayment);
            actions
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
    // The partition is over the whole registered pool, so the main-deck total
    // can range up to every card the player owns for this match.
    let pool_total = totals
        .values()
        .try_fold(0u32, |total, count| total.checked_add(*count));
    let Some(max_main_total) = pool_total else {
        return Err(InteractionReasonCode::PayloadTooLarge);
    };
    // CR 100.2a / CR 100.4a: `handle_submit_sideboard` is the authority; take
    // its bounds verbatim so this projection can never be stricter than what
    // the engine will actually accept. Because the pool is invariant, the
    // sideboard cap is equivalent to a floor on the main-deck total.
    let (min_main_deck_size, max_sideboard_size) =
        crate::game::match_flow::sideboard_submission_bounds(state, semantic_owner);
    let min_main_total = match max_sideboard_size {
        Some(max) => min_main_deck_size.max(max_main_total.saturating_sub(max)),
        None => min_main_deck_size,
    };
    let cards = totals
        .into_iter()
        .map(|(name, total)| SideboardCardProjection {
            current_main: current_main.get(name.as_str()).copied().unwrap_or(0),
            name,
            total,
        })
        .collect();
    Ok(Some(SideboardProjection {
        cards,
        min_main_total,
        max_main_total,
    }))
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

/// Rename `game::derived_views::UnboundedFamily` into its projection-layer code.
///
/// A pure rename on purpose: `family_of` stays the SINGLE authority for which axis groups
/// into which family, so this function makes no grouping decision of its own and cannot
/// drift from it. Exhaustive with no wildcard — a new family must choose a code here.
///
/// Mirrors `comparator_dto` above: the engine owns the fact, and the projection layer owns
/// the name it crosses the wire under.
fn preview_family(family: UnboundedFamily) -> InteractionShortcutPreviewFamily {
    match family {
        UnboundedFamily::Mana => InteractionShortcutPreviewFamily::Mana,
        UnboundedFamily::Life => InteractionShortcutPreviewFamily::Life,
        UnboundedFamily::Damage => InteractionShortcutPreviewFamily::Damage,
        UnboundedFamily::Mill => InteractionShortcutPreviewFamily::Mill,
        UnboundedFamily::Counters => InteractionShortcutPreviewFamily::Counters,
        UnboundedFamily::Tokens => InteractionShortcutPreviewFamily::Tokens,
        UnboundedFamily::Cards => InteractionShortcutPreviewFamily::Cards,
        UnboundedFamily::Casts => InteractionShortcutPreviewFamily::Casts,
        UnboundedFamily::Combats => InteractionShortcutPreviewFamily::Combats,
        UnboundedFamily::Turns => InteractionShortcutPreviewFamily::Turns,
        UnboundedFamily::Triggers => InteractionShortcutPreviewFamily::Triggers,
    }
}

/// The single site at which a resource axis becomes a published preview key.
///
/// Both halves are `derived_views`' decisions, not this layer's: `family_of` owns the
/// grouping and `payload_seat` owns the seat. The seat in particular is NOT keyed from the
/// proposer — a drain's magnitude belongs to the player LOSING the life — and sharing the
/// authority with `attribution_player` is what keeps the offer from attributing a seat the
/// HUD badge does not.
///
/// Named because the per-cycle fold and the victim re-attribution both derive it; spelling a
/// family literally at either site is how the two authorities drift apart.
fn preview_key(
    axis: crate::analysis::resource::ResourceAxis,
) -> (InteractionShortcutPreviewFamily, Option<u8>) {
    (
        preview_family(family_of(axis)),
        payload_seat(axis).map(|player| player.0),
    )
}

/// CR 732.2a: the finished magnitude of repeating `count` cycles of a measured per-period
/// delta — "the predictable results of the sequence of choices", stated per display family
/// and per affected seat.
///
/// **This is arithmetic over the certificate's `per_cycle.delta`, and nothing else.** It
/// applies no game action, resolves nothing, and touches no `GameState`: the multiplication
/// `n × δ` is the whole computation. In particular it is NOT
/// `interaction::preview_interaction`, which answers a different question (is this response
/// submittable) by cloning the state and applying to the clone. A clone-apply cannot answer
/// this one anyway — the count may be up to `MAX_SHORTCUT_CYCLES`, and the point of a CR
/// 732.2a shortcut is that the sequence is *not* played out.
///
/// The fold is over families, not axes: `ResourceVector` distinguishes mana by color and
/// counters by `(kind, bearer class)`, and summing those into one labelled magnitude per seat
/// is the aggregation the display layer is forbidden to do for itself. Losses are included
/// (signed), which is why this reads `axis_components()` rather than `unbounded_components()` —
/// the latter reports only what a cycle accrues, so a lethal drain would preview as nothing.
///
/// ponytail: magnitudes clamp to `i32`, so a per-cycle delta above ~2.1M would be reported
/// short. No such delta exists — one period's delta is a difference of two game-state
/// readings — and `i32` is exact in the JS number the binding generates, which `i64` is not.
fn shortcut_preview_entries(
    delta: &crate::analysis::resource::ResourceVector,
    count: u32,
    victim: Option<&VictimSplit>,
) -> Vec<InteractionShortcutPreviewEntry> {
    let charged_axis =
        victim.map(|split| crate::analysis::resource::ResourceAxis::Life(split.charged_seat));
    let mut per_cycle_totals: BTreeMap<(InteractionShortcutPreviewFamily, Option<u8>), i64> =
        BTreeMap::new();
    for (axis, magnitude) in delta.axis_components() {
        // The charged seat's life is re-added below, spread across the seats the declaration
        // allocates the count to. Folding it here as well would state it twice.
        if Some(axis) == charged_axis {
            continue;
        }
        let total = per_cycle_totals.entry(preview_key(axis)).or_insert(0);
        *total = total.saturating_add(magnitude);
    }
    let mut totals: BTreeMap<(InteractionShortcutPreviewFamily, Option<u8>), i64> =
        per_cycle_totals
            .into_iter()
            .map(|(key, per_cycle)| (key, per_cycle.saturating_mul(i64::from(count))))
            .collect();
    // CR 119.3: the announced slot charges a LIFE magnitude, so the re-attribution moves the
    // `Life` family and nothing else. `DamageDealt`, `LibraryDelta` and `Poison` are seat-keyed
    // by `payload_seat` too, and keep the seat it gave them.
    if let Some(split) = victim {
        for (seat, cycles) in &split.cycles {
            let key = preview_key(crate::analysis::resource::ResourceAxis::Life(*seat));
            let total = totals.entry(key).or_insert(0);
            *total = total.saturating_sub(split.rate.saturating_mul(i64::from(*cycles)));
        }
    }
    totals
        .into_iter()
        .filter_map(|((family, player), amount)| {
            // Families that cancel to zero across their axes (a cycle that gains and spends
            // the same mana) state nothing and are dropped rather than shown as `0`.
            (amount != 0).then_some(InteractionShortcutPreviewEntry {
                family,
                player,
                amount: amount.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            })
        })
        .collect()
}

/// CR 732.2a: the counts one offer publishes magnitudes for — a bounded sample of the
/// picker's own window, not the whole window.
///
/// The window's three stated axes (`min`, `suggested`, `max`) are always in the sample, so
/// the count the picker opens on always has an element and both endpoints are readable. The
/// interior is thinned by a stride wide enough that the whole sample fits under
/// `MAX_SHORTCUT_PREVIEW_ELEMENTS`; the length guard, not the stride, is what enforces the cap.
///
/// `step` starts at 1 rather than 0 deliberately: at 0 the loop regenerates `min` itself, and the
/// explicit `min` seed would then be unfalsifiable.
///
/// The exhaustive match is also the offer's single finite-count gate — `UntilLethal` names no
/// number to multiply a period by, so it yields no sample and the caller publishes nothing.
fn shortcut_preview_counts(count: &InteractionShortcutCountSpec) -> Vec<u32> {
    match count {
        InteractionShortcutCountSpec::UntilLethal => Vec::new(),
        InteractionShortcutCountSpec::Fixed {
            min,
            max,
            suggested,
        } => {
            let mut counts: std::collections::BTreeSet<u32> =
                [*min, *suggested, *max].into_iter().collect();
            let stride = u32::try_from(
                usize::try_from(max.saturating_sub(*min))
                    .unwrap_or(usize::MAX)
                    .div_ceil(MAX_SHORTCUT_PREVIEW_ELEMENTS)
                    .max(1),
            )
            .unwrap_or(u32::MAX);
            let mut step = 1u32;
            while counts.len() < MAX_SHORTCUT_PREVIEW_ELEMENTS {
                let Some(next) = stride
                    .checked_mul(step)
                    .and_then(|offset| min.checked_add(offset))
                    .filter(|next| next < max)
                else {
                    break;
                };
                counts.insert(next);
                step += 1;
            }
            counts.into_iter().collect()
        }
    }
}

/// CR 732.2a + CR 601.2c: the even split of one element's `count` over the announced choices
/// the declaration names, remainder on the EARLIEST ids so the amounts are non-increasing.
///
/// Empty ids or a zero count return before any division — the first because a `Targets` point
/// with no candidates is an admitted offer shape, the second because dividing by nothing is
/// not a split. Truncating to `count` when the candidates outnumber it is load-bearing: an
/// untruncated split mints a zero segment, and a zero segment names a cycle no seat absorbs.
fn canonical_allocation(ids: &[InteractionChoiceId], count: u32) -> Vec<AmountAssignment> {
    if ids.is_empty() || count == 0 {
        return Vec::new();
    }
    let parts = u32::try_from(ids.len()).unwrap_or(u32::MAX).min(count);
    let base = count / parts;
    let remainder = count % parts;
    ids.iter()
        .take(usize::try_from(parts).unwrap_or(usize::MAX))
        .enumerate()
        .map(|(index, choice_id)| AmountAssignment {
            choice_id: choice_id.clone(),
            amount: if u32::try_from(index).is_ok_and(|index| index < remainder) {
                base + 1
            } else {
                base
            },
        })
        .collect()
}

/// CR 119.3: the life magnitude one repetition charges through an announced slot, and the
/// single seat that magnitude currently lands on.
#[derive(Debug, Clone, Copy)]
struct VictimCharge {
    rate: i64,
    seat: PlayerId,
}

/// CR 119.3: what the period charges through THIS announced slot, or `None` when the period
/// does not say.
///
/// **A `victim_slot` magnitude is an aggregate, not a per-slot charge.** Every entry carries
/// the same number — the worst single seat's life loss over the whole period — so no victim
/// identity is recoverable from it, and reading it as one seat's charge names whichever seat
/// loses the most. This therefore resolves a victim only on the shape where the aggregate
/// provably IS the slot's charge: a period whose `delta.life` names exactly ONE losing seat,
/// and where that seat is one the declaration allocates to. Outside it the answer is `None`
/// and the fold keeps `payload_seat`'s own keys — an unsplit magnitude on the right seat
/// beats a split one on the wrong seats.
///
/// Deliberately NOT `analysis::resource::slot_charged_life`, which answers a different
/// question — what TOTAL is liftable, largest loss first, refusing a tie.
///
/// Four refusals, all fail-closed: no entry for the slot, a period whose life map names zero or
/// several losing seats, a losing seat the declaration does not announce, and a charge that is
/// not the whole of that seat's per-period loss.
///
/// A resolved charge is POSITIVE by construction rather than by a guard of its own: the sum
/// below is zero only when `rate` negates a magnitude the iterator already restricted to
/// losses. `checked_add` states that over the whole of `i64`, so there is no negation to
/// overflow and no positivity conjunct to keep.
///
/// That last one is the FOLD's precondition, not a second identification rule. CR 732.2a states
/// a count's magnitudes as the period times the count, and `shortcut_preview_entries` re-states
/// the charged seat's life by dropping that axis and re-adding `rate` once per allocated cycle.
/// The substitution preserves the period's life total exactly when `rate` is the seat's whole
/// loss; under any other charge the published magnitudes total less than the drain the
/// declaration takes, and CR 119.3 is the number the player is deciding on. The equality is
/// tested on the already-identified seat rather than used to pick one — filtering the life map
/// by it would name whichever seat happened to match the aggregate.
fn victim_charge(
    periodic: &crate::analysis::resource::PeriodicDelta,
    point: &LoopShortcutPointProjection,
    seats: &[PlayerId],
) -> Option<VictimCharge> {
    let rate = periodic
        .victim_slot
        .iter()
        .find(|(slot, _)| *slot == point.slot)
        .map(|(_, magnitude)| *magnitude)?;
    let mut losing = periodic
        .delta
        .life
        .iter()
        .filter(|(_, magnitude)| **magnitude < 0);
    let (seat, loss) = losing.next()?;
    (losing.next().is_none() && seats.contains(seat) && loss.checked_add(rate) == Some(0))
        .then_some(VictimCharge { rate, seat: *seat })
}

/// CR 119.3: how one element's count spreads the charged life magnitude over the seats the
/// declaration allocates it to.
///
/// `cycles` is never empty — the one constructor filters that case away — so "a split with no
/// parts" is unrepresentable rather than a case the fold has to remember.
#[derive(Debug, Clone)]
struct VictimSplit {
    rate: i64,
    charged_seat: PlayerId,
    cycles: Vec<(PlayerId, u32)>,
}

impl VictimSplit {
    /// Each allocation segment names its seat by `choice_id`, resolved against the point's
    /// published ids in published order; `ids` and `seats` are the two parallel readings of
    /// that same order. A segment naming an id the point does not publish contributes
    /// nothing rather than shifting every later segment onto the wrong seat.
    fn new(
        charge: &VictimCharge,
        ids: &[InteractionChoiceId],
        seats: &[PlayerId],
        allocation: &[AmountAssignment],
    ) -> Option<Self> {
        Some(Self {
            rate: charge.rate,
            charged_seat: charge.seat,
            cycles: allocation
                .iter()
                .filter_map(|assignment| {
                    let index = ids.iter().position(|id| *id == assignment.choice_id)?;
                    Some((*seats.get(index)?, assignment.amount))
                })
                .collect(),
        })
        .filter(|split| !split.cycles.is_empty())
    }
}

/// CR 601.2c: the announced-target point the allocation is stated over — the FIRST one in
/// published order.
///
/// "First" rather than "the": nothing forbids an offer publishing two `Targets` points, and
/// a per-point allocation carrier is a separate design. A candidate-less first point is NOT
/// skipped to reach a later one either — skipping would silently move the domain to a second
/// point, which is worse than publishing no allocation at all.
///
/// Takes an ITERATOR of points rather than a whole projection, so the finished
/// `projection.points` and the declared-sequence decode's still-accumulating `(point, segment)`
/// pairs both reach THIS function — a second finder for the second carrier would fork the
/// authority this one is.
fn allocation_point<'a>(
    points: impl IntoIterator<Item = &'a LoopShortcutPointProjection>,
) -> Option<(u32, &'a LoopShortcutPointProjection)> {
    let (index, point) = points
        .into_iter()
        .enumerate()
        .find(|(_, point)| point.kind == InteractionShortcutPointKind::Targets)?;
    Some((u32::try_from(index).ok()?, point))
}

/// CR 119.3: the seats a point's candidates name, in published order, or `None` when any
/// candidate is not a player.
///
/// Exhaustive over the candidate kinds so a new one must decide for itself rather than being
/// silently admitted as a life-bearing seat.
fn allocated_seats(
    projection: &LoopShortcutProjection,
    point: &LoopShortcutPointProjection,
) -> Option<Vec<PlayerId>> {
    point
        .candidate_indices
        .iter()
        .map(|index| match projection.candidates.get(*index)? {
            LoopShortcutCandidateValue::Target(TargetRef::Player(seat)) => Some(*seat),
            LoopShortcutCandidateValue::Target(TargetRef::Object(_))
            | LoopShortcutCandidateValue::ConvokeObject(_)
            | LoopShortcutCandidateValue::Mode(_)
            | LoopShortcutCandidateValue::May(_)
            | LoopShortcutCandidateValue::Unless(_)
            | LoopShortcutCandidateValue::ManaColor(_) => None,
        })
        .collect()
}

/// CR 601.2c: the announced-target decision an allocation is stated over — its published index,
/// the point itself, its choice ids in published order, and the seats those ids name when every
/// one of them is a player.
///
/// Minted from the PROJECTION alone. Which decision an allocation partitions, and over which
/// ids, is a property of the declaration and not of the period, so a proposal carrying no
/// measured per-period signature still has a partition to state — see `declared_sequence_preview`.
///
/// The ids are minted through the same `interaction_choice_id` call `loop_shortcut_points` uses,
/// so an element's `choice_id`s and the point's `candidate_ids` are the same strings by
/// construction rather than by agreement.
struct ShortcutAllocationDomain<'a> {
    group: u32,
    point: &'a LoopShortcutPointProjection,
    ids: Vec<InteractionChoiceId>,
    seats: Option<Vec<PlayerId>>,
}

fn shortcut_allocation_domain<'a>(
    interaction_id: &InteractionId,
    projection: &'a LoopShortcutProjection,
) -> Option<ShortcutAllocationDomain<'a>> {
    let (group, point) = allocation_point(&projection.points)?;
    Some(ShortcutAllocationDomain {
        group,
        point,
        ids: point
            .candidate_indices
            .iter()
            .map(|index| interaction_choice_id(interaction_id, 'k', *index))
            .collect(),
        seats: allocated_seats(projection, point),
    })
}

/// Everything a previewed element needs that neither its count nor its allocation supplies.
///
/// The announced-choice half is `ShortcutAllocationDomain`, folded in flat: an offer publishing
/// no announced-target decision has no domain, and states its magnitudes unsplit.
struct ShortcutPreviewBasis<'a> {
    delta: &'a crate::analysis::resource::ResourceVector,
    group: Option<u32>,
    ids: Vec<InteractionChoiceId>,
    seats: Option<Vec<PlayerId>>,
    charge: Option<VictimCharge>,
}

/// CR 732.2a: the per-period signature and the announced-choice domain every previewed element
/// is minted over, resolved once per offer rather than once per count.
///
/// Refuses an offer carrying no measured per-period signature, and one whose period states
/// nothing on any family: a magnitude is the period times the count, so neither has anything
/// to multiply.
fn shortcut_preview_basis<'a>(
    interaction_id: &InteractionId,
    projection: &'a LoopShortcutProjection,
) -> Option<ShortcutPreviewBasis<'a>> {
    let periodic = projection.per_cycle.as_ref()?;
    if shortcut_preview_entries(&periodic.delta, 1, None).is_empty() {
        return None;
    }
    let (group, ids, seats, charge) = match shortcut_allocation_domain(interaction_id, projection) {
        Some(domain) => {
            let charge = domain
                .seats
                .as_deref()
                .and_then(|seats| victim_charge(periodic, domain.point, seats));
            (Some(domain.group), domain.ids, domain.seats, charge)
        }
        None => (None, Vec::new(), None, None),
    };
    Some(ShortcutPreviewBasis {
        delta: &periodic.delta,
        group,
        ids,
        seats,
        charge,
    })
}

/// CR 732.2a: one previewed element — the basis multiplied by `count`, re-attributed over the
/// allocation this element states.
///
/// CR 732.1b: the sequence is deliberately never performed, so this is `n x delta` and reaches
/// no `GameState`. The single site that mints an element, for the published list and for a
/// declared one alike, so the two cannot disagree.
///
/// A `None` basis is a declaration whose PARTITION is known and whose magnitudes are not: the
/// count and the allocation are still the proposer's own, and no magnitude is invented beside
/// them. Only the declared side passes one — an offer with no basis publishes no element at all.
fn shortcut_preview_element(
    basis: Option<&ShortcutPreviewBasis<'_>>,
    count: u32,
    allocation: Vec<AmountAssignment>,
) -> InteractionShortcutPreview {
    let entries =
        basis
            .map(|basis| {
                let split = basis.charge.as_ref().zip(basis.seats.as_deref()).and_then(
                    |(charge, seats)| VictimSplit::new(charge, &basis.ids, seats, &allocation),
                );
                shortcut_preview_entries(basis.delta, count, split.as_ref())
            })
            .unwrap_or_default();
    InteractionShortcutPreview {
        count,
        entries,
        allocation,
    }
}

/// CR 732.2a: what each sampled count actually DOES, published as one element per count.
///
/// Three things have to hold before there is anything to state, and all three are the offer's
/// own: a measured per-period signature, a finite count window, and a period that states
/// something at all (a period netting to nothing on every family publishes no element at any
/// count, because `amount` is the period times the count).
fn loop_shortcut_preview(
    interaction_id: &InteractionId,
    projection: &LoopShortcutProjection,
) -> Vec<InteractionShortcutPreview> {
    let counts = shortcut_preview_counts(&projection.count);
    if counts.is_empty() {
        return Vec::new();
    }
    let Some(basis) = shortcut_preview_basis(interaction_id, projection) else {
        return Vec::new();
    };
    counts
        .into_iter()
        .map(|count| {
            let allocation = canonical_allocation(&basis.ids, count);
            shortcut_preview_element(Some(&basis), count, allocation)
        })
        .collect()
}

/// CR 732.2a: the previewed element for the declaration this response states — the same
/// arithmetic the published list carries, over the allocation the player authored.
///
/// CR 732.1b: the sequence is deliberately never performed, so this is `n x delta` and reaches
/// no `GameState`.
///
/// Fail-closed: a pin carrying no `amounts` states no split, so no element is minted for it
/// rather than one being invented from the canonical order. The count is not re-validated
/// here — an out-of-window count is refused by the ingress in the same call, and the payload
/// is attached only on the confirmable arm.
fn declared_shortcut_preview(
    waiting_for: &WaitingFor,
    interaction_id: &InteractionId,
    response: &InteractionResponse,
) -> Option<InteractionShortcutPreview> {
    let InteractionResponse::Shortcut { decision, pins } = response else {
        return None;
    };
    let projection = loop_shortcut_projection(waiting_for).ok()?;
    let count = match (*decision, projection.count) {
        (
            InteractionShortcutDecision::AcceptSuggested,
            InteractionShortcutCountSpec::Fixed { suggested, .. },
        ) => suggested,
        (
            InteractionShortcutDecision::Fixed { iterations },
            InteractionShortcutCountSpec::Fixed { .. },
        ) => iterations,
        (InteractionShortcutDecision::Decline, _)
        | (
            InteractionShortcutDecision::AcceptSuggested,
            InteractionShortcutCountSpec::UntilLethal,
        )
        | (InteractionShortcutDecision::Fixed { .. }, InteractionShortcutCountSpec::UntilLethal) => {
            return None
        }
    };
    let basis = shortcut_preview_basis(interaction_id, &projection)?;
    let group = basis.group?;
    let amounts = pins
        .iter()
        .find(|pin| pin.group == group)
        .map(|pin| pin.amounts.clone())?;
    if amounts.is_empty() {
        return None;
    }
    Some(shortcut_preview_element(Some(&basis), count, amounts))
}

/// CR 400.7: a live object's identity is incarnation-keyed and names something to surface;
/// `AllCopies` is a CARD identity naming no live object, so no candidate can be minted for it.
///
/// The ONE mapper, so the two arms that publish a `DecisionSource` cannot disagree about what a
/// card identity maps to. What a `None` COSTS is the caller's decision and is NOT the same at
/// both arms — see the publication posture on `declared_shortcut_projection`.
fn declared_object_candidate(
    source: &crate::analysis::decision_template::DecisionSource,
) -> Option<LoopShortcutCandidateValue> {
    match source {
        crate::types::game_state::YieldTarget::ThisObject { source_id, .. } => Some(
            LoopShortcutCandidateValue::Target(TargetRef::Object(*source_id)),
        ),
        crate::types::game_state::YieldTarget::AllCopies { .. } => None,
    }
}

/// CR 732.2a: the announcement sequence a scheduled pin's drive performs — one subject per step,
/// in the order the drive reaches them, beside the iteration each step starts at.
///
/// ONE rule for every schedule shape: `evaluate_schedule` resolves `Ranking::head` and never
/// advances past it, so no iteration announces a ranking's tail — reaching it mid-drive would be
/// the conditional action the rule bars, and `declaration_conforms` refuses a tail at declare
/// time, leaving a decoded save as the only way one arrives here. A constant
/// IS the one-step schedule starting at zero, so both shapes read through this one walk rather
/// than through arms that can drift apart.
///
/// `None` is a schedule this vocabulary cannot state: a cyclic one, which names no single
/// announcement order, and — a restored save being a wire ingress — a schedule with no step at
/// all, or starts that do not begin at zero or do not strictly increase, none of which is a
/// partition anybody declared. A stepless schedule announces nothing and cannot drive at all
/// (`evaluate_schedule` answers `ScheduleExhausted` for it), so an empty announcement sequence
/// would be a statement standing in for a refusal.
fn scheduled_announcements(
    schedule: &crate::analysis::decision_template::TargetSchedule,
) -> Option<(
    Vec<crate::analysis::decision_template::AnnouncementSubject>,
    Vec<u32>,
)> {
    use crate::analysis::decision_template::{Ranking, TargetSchedule};

    let steps: Vec<(u32, &Ranking)> = match schedule {
        TargetSchedule::Constant(ranking) => vec![(0, ranking)],
        TargetSchedule::Piecewise(steps) => steps
            .iter()
            .map(|(start, ranking)| (*start, ranking))
            .collect(),
        TargetSchedule::RoundRobin(_) => return None,
    };
    if steps.is_empty() {
        return None;
    }
    let mut subjects = Vec::with_capacity(steps.len());
    let mut starts = Vec::with_capacity(steps.len());
    for (start, ranking) in steps {
        match starts.last() {
            None if start != 0 => return None,
            Some(previous) if start <= *previous => return None,
            _ => {}
        }
        starts.push(start);
        subjects.push(ranking.head().clone());
    }
    Some((subjects, starts))
}

/// CR 601.2c + CR 732.1b: the segment lengths a declared count partitions into over the
/// iterations a decision's announcements start at — successive differences, the last running to
/// the count.
///
/// The one place that rule is expressed, for every pin kind. An until-lethal proposal names no
/// count to partition, so it declares an ORDER and no segment. `None` is a start past the count,
/// which is a partition nobody could have declared.
fn declared_segments(starts: &[u32], declared_count: Option<u32>) -> Option<Vec<u32>> {
    let Some(count) = declared_count else {
        return Some(Vec::new());
    };
    starts
        .iter()
        .zip(starts.iter().skip(1).copied().chain(std::iter::once(count)))
        .map(|(start, next)| next.checked_sub(*start))
        .collect()
}

/// CR 601.2c: what ONE announced-target decision states — its announcement subjects as published
/// candidate values, in the order the drive performs them, and the segment lengths the declared
/// count partitions into.
///
/// `None` is a decision this vocabulary cannot state: a multi-position slot, because one
/// announcement sequence answers one target position and a per-position carrier does not exist
/// here; a schedule naming no single announcement order; and a subject naming a CARD identity,
/// which names no live object to mint a candidate from.
fn declared_targets_statement(
    targets: &[crate::analysis::decision_template::TargetPin],
    declared_count: Option<u32>,
) -> Option<(Vec<LoopShortcutCandidateValue>, Vec<u32>)> {
    use crate::analysis::decision_template::{AnnouncementSubject, TargetPin};

    let [pin] = targets else { return None };
    let (subjects, starts) = match pin {
        // A pin naming ONE subject announces it from the first iteration on, which is the
        // one-step schedule the walk above mints for a constant.
        TargetPin::ByIdentity(source) => {
            (vec![AnnouncementSubject::Object(source.clone())], vec![0])
        }
        TargetPin::Player(seat) => (vec![AnnouncementSubject::Seat(*seat)], vec![0]),
        TargetPin::Scheduled(schedule) => scheduled_announcements(schedule)?,
    };
    let values = subjects
        .iter()
        .map(|subject| match subject {
            // CR 115.10a vs CR 601.2c: the two seat classes are two legality questions, and this
            // publishes neither. A seat surface states who was NAMED, never whether they are
            // still a legal target.
            AnnouncementSubject::Seat(seat) => {
                Some(LoopShortcutCandidateValue::Target(TargetRef::Player(*seat)))
            }
            AnnouncementSubject::Object(source) => declared_object_candidate(source),
        })
        .collect::<Option<Vec<_>>>()?;
    Some((values, declared_segments(&starts, declared_count)?))
}

/// CR 732.2b: the declaration a responder is being asked to accept or shorten, decoded into the
/// OFFER side's own projection vocabulary so every surface below it is minted by the offer's
/// producers rather than by a second copy of them.
///
/// # The publication posture, stated once for all seven pin kinds
///
/// A decision this projection can state nothing about publishes NO POINT, and the rest of the
/// declaration publishes anyway — `Mode`, `UnlessBreak`, `ManaColor`, `ConvokeTaps`, `Order`, an
/// unmintable `MayChoice` and an unstatable `Targets` decision alike, each for its own reason.
///
/// The ONE thing a skip may not do is move the ALLOCATION'S DOMAIN. `allocation_point` names the
/// FIRST published `Targets` point, so what a skipped announced-target decision owes is not
/// whether a domain is already fixed but whether a LATER decision ends up published in the slot
/// this one would have owned — which nothing knows until the walk ends, and which is therefore
/// asked there. A skip taken while that slot was still open refuses the whole sequence only if a
/// `Targets` point did go on to fill it; a skip behind an already-published one moves nothing,
/// and neither does one on a declaration that publishes no `Targets` point at all. Every skip
/// that moves no domain costs one statement line rather than the partition, the magnitudes and
/// every unrelated decision beside it. Both halves of the question are asked of
/// `allocation_point` itself, so the skip and the domain cannot answer it differently.
///
/// The matches over `PinnedDecision` here, and over `TargetPin` and `TargetSchedule` in the
/// statement helpers above, are wildcard-free, so a future variant carrying an identity or an
/// answer gets a compile-time visit rather than a silent omission.
fn declared_shortcut_projection(waiting_for: &WaitingFor) -> Option<DeclaredSequence> {
    use crate::analysis::decision_template::{IterationCount, PinnedDecision};

    let WaitingFor::RespondToShortcut { proposal, .. } = waiting_for else {
        return None;
    };
    // The single hidden-information authority has already set this to `None` for a viewer who
    // may not see the declaration, so an identity that authority dropped is unreachable here.
    let template = proposal.template.as_ref()?;
    // The DECLARED count as a degenerate one-point window. Nothing on this path samples a count
    // — the count sampler is never called here — so stating the declared count is the honest
    // value for a field the offer-side projection requires.
    let count = match proposal.count {
        IterationCount::Fixed(iterations) => InteractionShortcutCountSpec::Fixed {
            min: iterations,
            max: iterations,
            suggested: iterations,
        },
        IterationCount::UntilLethal => InteractionShortcutCountSpec::UntilLethal,
    };
    let declared_count = match proposal.count {
        IterationCount::Fixed(iterations) => Some(iterations),
        // CR 732.1b: an until-lethal proposal names no count to partition, so every arm below
        // declares an ORDER and no segment.
        IterationCount::UntilLethal => None,
    };

    let mut candidates: Vec<LoopShortcutCandidateValue> = Vec::new();
    // ONE accumulator, `unzip`ped at the single site below: a point cannot be recorded without
    // the segment entry `DeclaredSequence` aligns it with, so that alignment is a fact the
    // compiler holds rather than a pairing two parallel pushes have to remember.
    let mut published: Vec<(LoopShortcutPointProjection, Vec<u32>)> = Vec::new();
    let mut skipped_before_domain_fixed = false;
    for decision in &template.decisions {
        match decision {
            PinnedDecision::Targets { slot, targets } => {
                let Some((values, declared)) = declared_targets_statement(targets, declared_count)
                else {
                    // Whether this skip moves the domain depends on what the REST of the walk
                    // publishes, so record that it was taken with the domain slot still open and
                    // settle it below.
                    skipped_before_domain_fixed |=
                        allocation_point(published.iter().map(|(point, _)| point)).is_none();
                    continue;
                };
                let start = candidates.len();
                candidates.extend(values);
                published.push((
                    LoopShortcutPointProjection {
                        slot: slot.clone(),
                        kind: InteractionShortcutPointKind::Targets,
                        min: 0,
                        max: 0,
                        unique: true,
                        ordered: true,
                        read_only: true,
                        candidate_indices: (start..candidates.len()).collect(),
                    },
                    declared,
                ));
            }
            PinnedDecision::MayChoice { slot, take } => {
                // CR 603.5 (for the triggered-ability gates it governs) + CR 732.2c: an answered
                // optional decision is published as a STATEMENT, not an option set — `read_only`
                // is true, so no client picks from this list. Its two candidates are read IN
                // ORDER as the decision's SUBJECT and the way it went, and the arity is fixed by
                // this array.
                //
                // A slot source naming a CARD identity mints no subject, so this decision states
                // nothing and publishes NO POINT — the same answer `Mode`, `UnlessBreak`,
                // `ManaColor`, `ConvokeTaps` and `Order` give below, and the declaration's other
                // decisions are unaffected. `AllCopies` is not hidden information
                // (`game::visibility`'s redaction arm answers `false` for it: a card identity,
                // not a live object), so nothing here is a redaction and the all-or-nothing rule
                // that governs a REDACTED template does not reach this arm. Refusing the whole
                // sequence here would cost the responder the partition and the magnitudes to
                // save one statement line.
                let Some(subject) = declared_object_candidate(&slot.source) else {
                    continue;
                };
                let pair = [subject, LoopShortcutCandidateValue::May(*take)];
                let start = candidates.len();
                candidates.extend(pair);
                published.push((
                    LoopShortcutPointProjection {
                        slot: slot.clone(),
                        kind: InteractionShortcutPointKind::MayChoice,
                        min: 0,
                        max: 0,
                        unique: true,
                        ordered: true,
                        read_only: true,
                        candidate_indices: (start..candidates.len()).collect(),
                    },
                    Vec::new(),
                ));
            }
            // An offer carrying either of these keeps the count-only path, so no declaration
            // naming one reaches this decoder to render.
            PinnedDecision::Mode { .. } | PinnedDecision::UnlessBreak { .. } => {}
            // A latched constant rather than an answered choice: `validate_pins` states the
            // distinction — a "may" is yes/no, a mana color is a constant — so it is none of the
            // three things this projection publishes. The offer side does publish it as a
            // read-only point, so on such a board the responder sees strictly less than the
            // proposer; that cost is tracked as "Responder sees strictly less than the proposer
            // for `ManaColor` decisions".
            PinnedDecision::ManaColor { .. } => {}
            // Its own doc records that the concrete creatures are re-bound LIVE each iteration
            // and no per-iteration creature is latched, so the pin states no answer to publish.
            PinnedDecision::ConvokeTaps { .. } => {}
            // CR 603.3b trigger ordering under the static replay mode, not a loop-shortcut
            // per-iteration decision — and the one variant with no `slot`.
            PinnedDecision::Order { .. } => {}
        }
    }
    // CR 601.2c: the domain MOVED exactly when a skip was taken with the slot open and a later
    // decision then filled it — that point is now standing where the skipped decision's own
    // subjects would have been, which is worse than publishing no allocation at all.
    if skipped_before_domain_fixed
        && allocation_point(published.iter().map(|(point, _)| point)).is_some()
    {
        return None;
    }
    if published.is_empty() {
        return None;
    }
    let (points, segments) = published.into_iter().unzip();
    Some(DeclaredSequence {
        projection: LoopShortcutProjection {
            count,
            per_cycle: proposal.per_cycle.clone(),
            points,
            candidates,
        },
        segments,
    })
}

/// CR 732.2a: the previewed element for the declaration a responder is being asked to accept —
/// the same arithmetic the offer's published list carries, over the allocation the proposer
/// declared.
///
/// CR 732.1b: the sequence is deliberately never performed, so this is `n x delta` and reaches
/// no game state.
///
/// `None` is reserved for a declaration whose PARTITION cannot be stated: no announced-target
/// decision to allocate over, or a segment list this projection cannot read back against that
/// decision's own published ids and total. Every refusal on the MAGNITUDE leg publishes the
/// partition with no entries instead — segment lengths are not magnitudes, and a responder
/// judging accept-or-shorten against half the proposal is the partial statement this projection
/// exists to rule out.
///
/// The domain's `group` travels OUT with the element, so a reader identifies which announced
/// decision the allocation partitions instead of inferring it from the element's presence or
/// from the point's position. One `Option` carries both, so the group cannot be published for
/// an element that was refused, nor withheld from one that was stated.
fn declared_sequence_preview(
    interaction_id: &InteractionId,
    declared: &DeclaredSequence,
) -> Option<(InteractionShortcutPreview, u32)> {
    // CR 601.2c: `allocation_point` is the single authority for WHICH announced-target point an
    // allocation is stated over, and the domain's `group`/`ids` are already that point's. Reading
    // the segments back by that same index is what makes the two halves of this element speak
    // about one point: a proposal may carry more than one announced-target decision, and every
    // later one publishes its declared ORDER with no allocation stated over it.
    let domain = shortcut_allocation_domain(interaction_id, &declared.projection)?;
    let index = usize::try_from(domain.group).ok()?;
    let segments = declared.segments.get(index)?;
    if segments.is_empty() || segments.len() != domain.ids.len() {
        return None;
    }

    // A restored save is a wire ingress, so the declared total fails closed rather than wrapping.
    let count = segments
        .iter()
        .try_fold(0u32, |total, segment| total.checked_add(*segment))?;
    let allocation: Vec<AmountAssignment> = domain
        .ids
        .iter()
        .zip(segments)
        .map(|(choice_id, amount)| AmountAssignment {
            choice_id: choice_id.clone(),
            amount: *amount,
        })
        .collect();

    // CR 119.3: the period charges this announced slot's life to whoever the DECLARATION names.
    //
    // `victim_charge` refuses for FOUR reasons: no `victim_slot` entry for this point's slot; a
    // life map naming zero or several losing seats; a losing seat the seats-in-hand do not name;
    // and a charge that is not the whole of that seat's per-period loss. Only the THIRD is about
    // this call site's narrower domain — `basis.seats` here is the declaration's announced
    // subjects, a subset of the offer's candidate domain. On the other three the offer's own
    // element refuses in exactly the same way, so both sides fold the period's seat keys and
    // stating the magnitudes is correct.
    //
    // So re-ask the SAME landed rule over the life map's OWN KEYS. That domain satisfies the
    // third conjunct by construction (the seat it tests is drawn from that very map), leaving the
    // other three deciding — a charge it resolves that `basis.seats` does not name is a seat this
    // declaration never announces. Folding it without a split would key the whole drain on the
    // seat the period was MEASURED on, so state NO MAGNITUDE rather than one the responder would
    // judge their accept-or-shorten answer on. The partition is published either way.
    //
    // The `basis.seats` wrapper covers the remaining case: a declaration whose announced subjects
    // are OBJECTS has no seat domain at all, so nothing narrowed and the offer's own fold over
    // the period's seat keys is right for it too.
    //
    // The `per_cycle` conjunct re-reads a value `shortcut_preview_basis` already required, so it
    // is `Some` on every basis that resolved; were it not, the zip would drop the magnitudes,
    // which is the direction this guard already fails in.
    let basis = shortcut_preview_basis(interaction_id, &declared.projection);
    let charge_escapes_declaration = basis
        .as_ref()
        .zip(declared.projection.per_cycle.as_ref())
        .is_some_and(|(basis, periodic)| {
            basis.seats.as_deref().is_some_and(|announced| {
                let life_seats: Vec<PlayerId> = periodic.delta.life.keys().copied().collect();
                victim_charge(periodic, domain.point, &life_seats)
                    .is_some_and(|charge| !announced.contains(&charge.seat))
            })
        });
    Some((
        shortcut_preview_element(
            basis.as_ref().filter(|_| !charge_escapes_declaration),
            count,
            allocation,
        ),
        domain.group,
    ))
}

fn loop_shortcut_projection(
    waiting_for: &WaitingFor,
) -> Result<LoopShortcutProjection, InteractionReasonCode> {
    use crate::analysis::decision_template::DecisionPointKind;

    let WaitingFor::LoopShortcut {
        schema,
        certificate,
        ..
    } = waiting_for
    else {
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
            // CR 732.2a: the picker's ceiling is the offer's own CR 704 bound, never the raw
            // global safety limit — a count above it would specify a sequence containing an
            // elimination, which is a conditional action. The engine owns this number; the
            // frontend renders it. An unnarrowed offer states `MAX_SHORTCUT_CYCLES`; a bounded
            // offer states less. Either way this is the offer's own bound, clamped at the same
            // authority.
            //
            // CR 704.5a: `elimination_bounds` returns `0` to mean "no legal repetition exists and
            // the caller must not offer". A published offer carrying `0` is an authority
            // violation, not a number to repair — clamping it to `1` renders a one-iteration
            // offer whose single iteration eliminates a player mid-proposal. Reject it in EVERY
            // build: a `debug_assert!` disappears from release, which is precisely where the
            // clamp is what the player sees.
            //
            // THIS GUARD IS ALSO LOAD-BEARING AGAINST A PANIC, not merely against a bad
            // offer. With the lower clamp replaced by `.min(MAX_SHORTCUT_CYCLES)` below,
            // a `0` authority yields `max == 0`, and `suggested.clamp(1, max)` is then
            // `Ord::clamp(1, 0)`, whose `assert!(min <= max)` is a PLAIN assert that
            // survives release (measured: an `-O` build of `5u32.clamp(1, 0)` panics with
            // `min > max. min = 1, max = 0`). Removing this guard turns a malformed
            // restored dump into an engine panic.
            //
            // LATENT, NOT LIVE (measured at this head): no in-tree producer can reach this
            // arm with `0`. `build_shortcut_schema` (`game/engine.rs`) has THREE call sites:
            // `interactive_loop_bridge` and `try_offer_object_growth_shortcut` pass
            // `MAX_SHORTCUT_CYCLES`, while `certified_bounded_cycle_offer` passes a NARROWED
            // `max_iterations` — which cannot be `0` either, because that producer refuses
            // outright unless `(1..MAX_SHORTCUT_CYCLES).contains(&max_iterations)`. The
            // per-viewer projection in `game/visibility.rs` only re-projects an existing
            // schema's value; and
            // `ShortcutDecisionSchema::default().max_iterations == default_max_iterations()
            // == MAX_SHORTCUT_CYCLES` (`analysis/decision_template.rs`), which is also the
            // `#[serde(default)]` for a pre-bound save. The only way `0`
            // arrives is a LOADED/PERSISTED authority that explicitly serializes it. This
            // guard is therefore the fail-closed twin of item E: a latent hole shut before
            // it opens.
            if schema.max_iterations == 0 {
                return Err(InteractionReasonCode::InvalidAuthorityState);
            }
            let max = schema.max_iterations.min(MAX_SHORTCUT_CYCLES);
            InteractionShortcutCountSpec::Fixed {
                min: 1,
                max,
                suggested: suggested.clamp(1, max),
            }
        }
        crate::analysis::decision_template::IterationCount::UntilLethal => {
            InteractionShortcutCountSpec::UntilLethal
        }
    };
    // CR 732.2a: carry the measured per-period signature forward so the picker's numbers can
    // state their consequence instead of standing alone. It is published only by the producer
    // that measured one (`certified_bounded_cycle_offer`); every other mint carries `None`, as
    // does every save written before the field existed. The other authority the magnitudes
    // need — a FINITE count — is `count` above, and the two coincide by construction rather
    // than by luck: the bounded producer both narrows `max_iterations` and mints
    // `Fixed(max_iterations)`.
    let per_cycle = certificate.per_cycle.clone();
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
        per_cycle,
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

/// CR 115.1 + CR 601.2c: Label a target announcement with what the announcing
/// spell or ability will do to the thing being chosen.
///
/// Mirrors `effect_zone_intent` above: the slot stores the game fact
/// (`EffectKind`), and this projection-layer function decides how to present
/// it. Nothing about the label is stored in `GameState`.
///
/// The catch-all is deliberate and safe. `EffectKind` has 231 variants, the
/// overwhelming majority of which never reach a CR 115.1 target slot, and the
/// fallback is the NEUTRAL `Choose` — so an unmapped or newly-added kind reads
/// as "just pick one", which is an honest partial rather than a wrong claim.
/// An exhaustive match here would be 231 arms whose default answer is the same
/// value, and would make every unrelated effect addition edit this function.
fn target_intent(effect_kind: EffectKind, detail: TargetEffectDetail) -> InteractionIntentCode {
    // The zone family's unit tag says only "a zone change"; the destination is
    // the deciding fact and is stamped on the slot. Delegate to the existing
    // zone labeller rather than reimplementing it, so `EffectZoneChoice` and a
    // zone-change target announcement can never disagree.
    if let TargetEffectDetail::Destination(destination) = detail {
        return effect_zone_intent(effect_kind, Some(destination));
    }
    match effect_kind {
        // CR 120.1: damage.
        EffectKind::DealDamage
        | EffectKind::DamageAll
        | EffectKind::DamageEachPlayer
        | EffectKind::ApplyPostReplacementDamage
        | EffectKind::EachDealsDamageEqualToPower
        | EffectKind::EachSourceDealsDamage => InteractionIntentCode::Damage,
        // CR 701.8: destroy.
        EffectKind::Destroy | EffectKind::DestroyAll => InteractionIntentCode::Destroy,
        // CR 701.19: regeneration shield. Unambiguously protective.
        EffectKind::Regenerate | EffectKind::RemoveAllDamage => InteractionIntentCode::Regenerate,
        // CR 701.6: counter a spell.
        EffectKind::Counter | EffectKind::CounterAll => InteractionIntentCode::Counter,
        // CR 701.21: sacrifice.
        EffectKind::Sacrifice | EffectKind::ChooseAndSacrificeRest | EffectKind::Exploit => {
            InteractionIntentCode::Sacrifice
        }
        // CR 701.26: tap / untap.
        EffectKind::Tap | EffectKind::TapAll => InteractionIntentCode::Tap,
        EffectKind::Untap | EffectKind::UntapAll => InteractionIntentCode::Untap,
        // CR 701.17: mill.
        EffectKind::Mill => InteractionIntentCode::Mill,
        // CR 701.9: discard.
        EffectKind::DiscardCard | EffectKind::Discard => InteractionIntentCode::Discard,
        // CR 121.1: draw.
        EffectKind::Draw => InteractionIntentCode::Draw,
        // CR 119.3: life gain / loss.
        EffectKind::GainLife => InteractionIntentCode::GainLife,
        EffectKind::LoseLife => InteractionIntentCode::LoseLife,
        // CR 701.14: fight.
        EffectKind::Fight => InteractionIntentCode::Fight,
        // CR 701.3: attach (Auras and Equipment).
        EffectKind::Attach | EffectKind::AttachAll | EffectKind::ReturnAsAura => {
            InteractionIntentCode::Attach
        }
        // CR 707: copy.
        EffectKind::CopySpell
        | EffectKind::EpicCopy
        | EffectKind::CastCopyOfCard
        | EffectKind::CopyTokenOf
        | EffectKind::BecomeCopy => InteractionIntentCode::Copy,
        // CR 613.1b: control change.
        EffectKind::GainControl
        | EffectKind::GainControlAll
        | EffectKind::ControlNextTurn
        | EffectKind::GiveControl
        | EffectKind::ExchangeControl => InteractionIntentCode::GainControl,
        // CR 701.20: reveal.
        EffectKind::Reveal | EffectKind::RevealUntil => InteractionIntentCode::Reveal,
        // CR 406.1: exile. Only the kinds that name exile in the tag itself
        // qualify. Plain "exile target creature" is `EffectKind::ChangeZone`,
        // whose destination lives in the `Effect` payload and not in the unit
        // tag, so it cannot be recognized here and stays neutral below — the
        // same shape as the `Pump` sign problem. `effect_zone_intent` solves
        // this for `EffectZoneChoice` only because that state stores the
        // destination alongside the kind.
        EffectKind::ExileHaunting | EffectKind::ExileTop | EffectKind::HeistExile => {
            InteractionIntentCode::Exile
        }
        // Return to hand ("bounce"). Not a keyword action, so no CR citation:
        // it is an ordinary zone change to the owner's hand.
        EffectKind::Bounce | EffectKind::BounceAll => InteractionIntentCode::Return,
        // CR 613.4: characteristic modification. Covers both directions —
        // `EffectKind` is a unit tag, so `Pump` is the same variant for
        // "+3/+3" and "-3/-3" (`Effect::Pump` carries the sign in `PtValue`,
        // and that sign can be dynamic). `Modify` therefore names the action
        // and claims no disposition; see the adapter's mapping for the
        // consequence at the protocol boundary.
        // CR 613.4: direction is read off `Effect::Pump`'s `PtValue` payload at
        // slot construction, because `EffectKind` cannot carry it. `Modify`
        // survives for the two populations where no direction is true: a
        // dynamic magnitude (X / count-based) and a genuinely opposing
        // modification ("+2/-2").
        EffectKind::Pump | EffectKind::PumpAll => match detail {
            TargetEffectDetail::Modification(PtDirection::Increase) => InteractionIntentCode::Buff,
            TargetEffectDetail::Modification(PtDirection::Decrease) => {
                InteractionIntentCode::Debuff
            }
            TargetEffectDetail::None | TargetEffectDetail::Destination(_) => {
                InteractionIntentCode::Modify
            }
        },
        EffectKind::SwitchPT
        | EffectKind::DoublePT
        | EffectKind::DoublePTAll
        | EffectKind::PutCounter
        | EffectKind::PutCounterAll
        | EffectKind::PutChosenCounter
        | EffectKind::RemoveCounter
        | EffectKind::MultiplyCounter
        | EffectKind::MoveCounters
        | EffectKind::Animate => InteractionIntentCode::Modify,
        // Everything else is either not reachable from a target slot or has no
        // effect-semantic disposition (e.g. `NoOp`, which the mutate and other
        // pipeline-resolved slots carry). Neutral by construction.
        _ => InteractionIntentCode::Choose,
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
        WaitingFor::ResolveAllConsent { .. } | WaitingFor::ResolveAllReady { .. } => None,
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
                    mode: TapCreaturesSelectionMode::Aggregate(aggregate),
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
                // CR 107.3a: both count-bounded forms project as a plain
                // `[min_count, count]` selection; the X-sentinel's freedom is
                // already encoded in the bounds the registration site published.
                PayCostKind::TapCreatures {
                    mode: TapCreaturesSelectionMode::Fixed | TapCreaturesSelectionMode::VariableX,
                }
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
        | WaitingFor::EntryAttackTargetChoice { .. }
        | WaitingFor::ManaPayment { .. }
        | WaitingFor::ManaSourceSelection { .. }
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
        | WaitingFor::EntryControllerChoice { .. }
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
        | WaitingFor::ResolutionOptionalPaymentChoice { .. }
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
        | WaitingFor::ChooseGiftRecipient { .. }
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

/// Stable, opaque identity for an exact serialized action. The interaction
/// projection and per-object action payload both use this, so consumers never
/// need to correlate semantic rows by array position.
pub fn interaction_action_id(action: &GameAction) -> InteractionActionId {
    let encoded = serde_json::to_vec(action).expect("GameAction serialization must succeed");
    let digest = Sha256::digest(encoded);
    InteractionActionId(format!("a{:x}", digest))
}

/// A per-object legal action with an engine-authored identity that can be
/// joined to an [`InteractionPresentationSurface::Action`] without depending
/// on either transport's row ordering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectActionPayload {
    #[serde(flatten)]
    pub action: GameAction,
    pub interaction_action_id: InteractionActionId,
}

pub fn object_action_payloads(
    actions: &HashMap<ObjectId, Vec<GameAction>>,
) -> HashMap<ObjectId, Vec<ObjectActionPayload>> {
    actions
        .iter()
        .map(|(object_id, object_actions)| {
            (
                *object_id,
                object_actions
                    .iter()
                    .cloned()
                    .map(|action| ObjectActionPayload {
                        interaction_action_id: interaction_action_id(&action),
                        action,
                    })
                    .collect(),
            )
        })
        .collect()
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

fn interaction_mana_comparator(comparator: Comparator) -> InteractionManaComparator {
    match comparator {
        Comparator::GT => InteractionManaComparator::GreaterThan,
        Comparator::LT => InteractionManaComparator::LessThan,
        Comparator::GE => InteractionManaComparator::AtLeast,
        Comparator::LE => InteractionManaComparator::AtMost,
        Comparator::EQ => InteractionManaComparator::Equal,
        Comparator::NE => InteractionManaComparator::NotEqual,
    }
}

fn interaction_mana_ability_activation_scope(
    scope: AbilityActivationScope,
) -> InteractionManaAbilityActivationScope {
    match scope {
        AbilityActivationScope::OfSpellType => InteractionManaAbilityActivationScope::OfSpellType,
        AbilityActivationScope::Any => InteractionManaAbilityActivationScope::Any,
    }
}

fn interaction_mana_special_action(action: SpecialAction) -> InteractionManaSpecialAction {
    match action {
        SpecialAction::CompanionToHand => InteractionManaSpecialAction::CompanionToHand,
        SpecialAction::UnlockDoor => InteractionManaSpecialAction::UnlockDoor,
        SpecialAction::Plot => InteractionManaSpecialAction::Plot,
        SpecialAction::TurnFaceUp => InteractionManaSpecialAction::TurnFaceUp,
        SpecialAction::RollPlanarDie => InteractionManaSpecialAction::RollPlanarDie,
        SpecialAction::EndContinuousEffect => InteractionManaSpecialAction::EndContinuousEffect,
    }
}

fn interaction_mana_spell_cost_criterion(
    criterion: &SpellCostCriterion,
) -> InteractionManaSpellCostCriterion {
    match criterion {
        SpellCostCriterion::ManaValue { comparator, value } => {
            InteractionManaSpellCostCriterion::ManaValue {
                comparator: interaction_mana_comparator(*comparator),
                value: *value,
            }
        }
        SpellCostCriterion::HasXInCost => InteractionManaSpellCostCriterion::HasXInCost,
    }
}

/// Uses the enum's Serde representation, which is the protocol's canonical
/// keyword vocabulary, instead of its unstable debug representation.
fn interaction_keyword_kind_code(keyword: crate::types::keywords::KeywordKind) -> String {
    serde_json::to_value(keyword)
        .expect("fieldless keyword kinds serialize")
        .as_str()
        .expect("fieldless keyword kinds serialize as strings")
        .to_owned()
}

fn interaction_mana_restriction(restriction: &ManaRestriction) -> InteractionManaRestriction {
    match restriction {
        ManaRestriction::OnlyForSpell => InteractionManaRestriction::OnlyForSpell,
        ManaRestriction::OnlyForSpellType(spell_type) => {
            InteractionManaRestriction::OnlyForSpellType {
                spell_type: spell_type.clone(),
            }
        }
        ManaRestriction::OnlyForCreatureType(creature_type) => {
            InteractionManaRestriction::OnlyForCreatureType {
                creature_type: creature_type.clone(),
            }
        }
        ManaRestriction::OnlyForTypeSpellsOrAbilities {
            spell_type,
            ability,
        } => InteractionManaRestriction::OnlyForTypeSpellsOrAbilities {
            spell_type: spell_type.clone(),
            ability: interaction_mana_ability_activation_scope(*ability),
        },
        ManaRestriction::OnlyForActivation => InteractionManaRestriction::OnlyForActivation,
        ManaRestriction::OnlyForTaggedActivation(tag) => {
            InteractionManaRestriction::OnlyForTaggedActivation {
                tag: tag.keyword_str().to_string(),
            }
        }
        ManaRestriction::OnlyForXCosts => InteractionManaRestriction::OnlyForXCosts,
        ManaRestriction::OnlyForSpellWithKeywordKind(keyword) => {
            InteractionManaRestriction::OnlyForSpellWithKeywordKind {
                keyword: interaction_keyword_kind_code(*keyword),
            }
        }
        ManaRestriction::OnlyForSpellWithKeywordKindFromZone(keyword, zone) => {
            InteractionManaRestriction::OnlyForSpellWithKeywordKindFromZone {
                keyword: interaction_keyword_kind_code(*keyword),
                zone: zone_code(*zone),
            }
        }
        ManaRestriction::OnlyForSpellWithManaValue { comparator, value } => {
            InteractionManaRestriction::OnlyForSpellWithManaValue {
                comparator: interaction_mana_comparator(*comparator),
                value: *value,
            }
        }
        ManaRestriction::OnlyForSpellMatchingCostCriteria {
            spell_type,
            criteria,
        } => InteractionManaRestriction::OnlyForSpellMatchingCostCriteria {
            spell_type: spell_type.clone(),
            criteria: criteria
                .iter()
                .map(interaction_mana_spell_cost_criterion)
                .collect(),
        },
        ManaRestriction::OnlyForSpellWithColorCount { comparator, count } => {
            InteractionManaRestriction::OnlyForSpellWithColorCount {
                comparator: interaction_mana_comparator(*comparator),
                count: *count,
            }
        }
        ManaRestriction::OnlyForSpellColor(color) => {
            InteractionManaRestriction::OnlyForSpellColor {
                color: mana_color_dto(*color),
            }
        }
        ManaRestriction::OnlyForSpellFromZone(zone_spend) => {
            InteractionManaRestriction::OnlyForSpellFromZone {
                zone: zone_code(zone_spend.zone),
                polarity: match zone_spend.polarity {
                    ZoneSpendPolarity::From => InteractionManaZoneSpendPolarity::From,
                    ZoneSpendPolarity::NotFrom => InteractionManaZoneSpendPolarity::NotFrom,
                },
            }
        }
        ManaRestriction::CannotCastSpellFromZone(zone) => {
            InteractionManaRestriction::CannotCastSpellFromZone {
                zone: zone_code(*zone),
            }
        }
        ManaRestriction::OnlyForFaceDownSpell => InteractionManaRestriction::OnlyForFaceDownSpell,
        ManaRestriction::OnlyForAny(restrictions) => InteractionManaRestriction::OnlyForAny {
            restrictions: restrictions
                .iter()
                .map(interaction_mana_restriction)
                .collect(),
        },
        ManaRestriction::OnlyForSpecialAction(action) => {
            InteractionManaRestriction::OnlyForSpecialAction {
                action: interaction_mana_special_action(*action),
            }
        }
        ManaRestriction::Impossible => InteractionManaRestriction::Impossible,
        ManaRestriction::ConvokePayment => InteractionManaRestriction::ConvokePayment,
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
        CastPaymentMode::AutoExceptSacrificialMana => "autoExceptSacrificialMana",
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

fn auto_may_scope_code(scope: MayTriggerAutoChoiceScope) -> &'static str {
    match scope {
        MayTriggerAutoChoiceScope::ExactInstance => "exactInstance",
        MayTriggerAutoChoiceScope::SameCard => "sameCard",
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

/// Label one mana action with the mana its own reducer would actually produce.
/// `resolve` is the caller-supplied authority — the exact function the reducer
/// for that action variant uses to revalidate the frozen selection — so a label
/// can never be derived through a sibling surface's selection form. A stale or
/// no-longer-legal selection resolves to no produced-mana surface, matching the
/// reducer's own refusal to activate it.
fn push_produced_mana_surfaces(
    surfaces: &mut Vec<InteractionPresentationSurface>,
    state: &GameState,
    selection: &ManaSourceSelection,
    resolve: fn(
        &GameState,
        PlayerId,
        &ManaSourceSelection,
    ) -> Result<mana_sources::ManaSourceOption, EngineError>,
) {
    let Some(player) = state
        .objects
        .get(&selection.source.object_id)
        .map(|object| object.controller)
    else {
        return;
    };
    let Ok(option) = resolve(state, player, selection) else {
        return;
    };
    for (index, unit) in mana_sources::live_mana_output_for_option(state, player, &option)
        .into_iter()
        .enumerate()
    {
        surfaces.push(InteractionPresentationSurface::Mana {
            role: InteractionRoleCode::ProducedMana,
            index: Some(index as u32),
            symbols: vec![mana_type_code(unit.mana_type).to_string()],
            restrictions: unit
                .restrictions
                .iter()
                .map(interaction_mana_restriction)
                .collect(),
        });
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
        | GameAction::BackToManaPayment
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
        // The two public mana-action surfaces carry deliberately different
        // selection forms, so each must be labelled through the same authority
        // that will execute it.
        //
        // `TapLandForMana` is minted by `activatable_mana_actions_for_player`
        // from `ManaSourceOption::semantic_selection` — one *concrete* row per
        // producible color — and is executed by `handle_tap_land_for_mana` via
        // `live_land_mana_option_for_selection`.
        //
        // `ActivateManaSource` is minted from
        // `activatable_mana_source_selections`, whose `manual_selection_for_option`
        // intentionally collapses a flexible source to `Colorless` +
        // `DeferredColorChoice` so the ordinary mana-choice resolver asks for the
        // color, and is executed by `activate_mana_source_selection` via
        // `live_mana_source_option_for_selection`.
        //
        // Resolving one through the other's authority can never match a flexible
        // source, which silently produced an unlabelled action (issue #6944:
        // City of Brass). Keep each arm paired with its own reducer's resolver.
        GameAction::TapLandForMana { selection } => push_produced_mana_surfaces(
            surfaces,
            state,
            selection,
            mana_sources::live_land_mana_option_for_selection,
        ),
        GameAction::ActivateManaSource { selection } => push_produced_mana_surfaces(
            surfaces,
            state,
            selection,
            mana_sources::live_mana_source_option_for_selection,
        ),
        GameAction::PlayLand { .. }
        | GameAction::Foretell { .. }
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
        | GameAction::ChooseAnnouncingOpponent { opponent }
        | GameAction::ChooseGiftRecipient { opponent }
        | GameAction::ChooseEntryController { opponent } => {
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
        GameAction::ChooseResolutionOptionalPaymentBranch { choice } => match choice {
            ResolutionOptionalPaymentChoice::Decline => {
                push_value_surface(surfaces, InteractionRoleCode::CostBranch, "decline")
            }
            ResolutionOptionalPaymentChoice::Pay { index } => {
                push_value_surface(surfaces, InteractionRoleCode::CostBranchIndex, index)
            }
        },
        GameAction::RespondToSpliceOffer { card } => {
            if let Some(card) = card {
                push_object_surface(surfaces, state, *card, InteractionRoleCode::SpliceCard);
            } else {
                push_value_surface(surfaces, InteractionRoleCode::Splice, "decline");
            }
        }
        GameAction::DecideOptionalEffectAndRemember { choice, scope } => push_value_surface(
            surfaces,
            InteractionRoleCode::Choice,
            format!(
                "{}:{}",
                auto_may_choice_code(*choice),
                auto_may_scope_code(*scope)
            ),
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
                restrictions: Vec::new(),
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
                restrictions: Vec::new(),
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
                restrictions: Vec::new(),
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
        GameAction::BeginResolveAll { .. } => {
            surfaces.push(InteractionPresentationSurface::ShortcutResponse {
                response: InteractionShortcutResponseCode::Propose,
            });
        }
        GameAction::RespondResolveAllConsent { decision, .. } => {
            let response = match decision {
                crate::types::actions::ResolveAllConsentDecision::Grant => {
                    InteractionShortcutResponseCode::Accept
                }
                crate::types::actions::ResolveAllConsentDecision::Decline => {
                    InteractionShortcutResponseCode::Decline
                }
            };
            surfaces.push(InteractionPresentationSurface::ShortcutResponse { response });
        }
        GameAction::RevokeResolveAllConsent { .. } => {
            surfaces.push(InteractionPresentationSurface::ShortcutResponse {
                response: InteractionShortcutResponseCode::Decline,
            });
        }
        // CR 116.2c: two live pay-to-end permissions are two distinct
        // candidates, so the group key must reach the surface list or they
        // project identically. The permanent whose resolution installed the
        // effect is the player-meaningful discriminator and is derived HERE, in
        // the engine, rather than being widened into the action payload.
        GameAction::EndContinuousEffect { group, .. } => {
            if let Some(source) = crate::game::end_continuous_effect::source_of_group(state, *group)
            {
                push_object_surface(
                    surfaces,
                    state,
                    source,
                    InteractionRoleCode::PermissionSource,
                );
            }
            push_value_surface(surfaces, InteractionRoleCode::Selected, group.0);
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
                        restrictions: Vec::new(),
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
                    restrictions: Vec::new(),
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
        GameAction::ChooseGiftRecipient { .. } => InteractionActionCode::ChooseGiftRecipient,
        GameAction::ChooseAssistPlayer { .. } => InteractionActionCode::ChooseAssistPlayer,
        GameAction::CommitAssistPayment { .. } => InteractionActionCode::CommitAssistPayment,
        GameAction::MulliganDecision { .. } => InteractionActionCode::MulliganDecision,
        GameAction::ReorderHand { .. } => InteractionActionCode::ReorderHand,
        GameAction::TapLandForMana { .. } => InteractionActionCode::TapLandForMana,
        GameAction::ActivateManaSource { .. } => InteractionActionCode::ActivateManaSource,
        GameAction::BackToManaPayment => InteractionActionCode::BackToManaPayment,
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
        GameAction::ChooseEntryController { .. } => InteractionActionCode::ChooseEntryController,
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
        GameAction::ChooseResolutionOptionalPaymentBranch { .. } => {
            InteractionActionCode::ChooseResolutionOptionalPaymentBranch
        }
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
        GameAction::EndContinuousEffect { .. } => InteractionActionCode::EndContinuousEffect,
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
        GameAction::BeginResolveAll { .. } => InteractionActionCode::DeclareShortcut,
        GameAction::RespondResolveAllConsent { .. } => InteractionActionCode::RespondToShortcut,
        GameAction::RevokeResolveAllConsent { .. } => InteractionActionCode::DeclineShortcut,
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
            action_id: Some(interaction_action_id(action)),
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
            if *payment_mode == CastPaymentMode::Manual {
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
                            restrictions: Vec::new(),
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
                        restrictions: Vec::new(),
                    });
                }
                ManaGroupCandidateValue::Phyrexian { choice, color } => {
                    surfaces.push(InteractionPresentationSurface::Mana {
                        role: InteractionRoleCode::PhyrexianPayment,
                        index: Some(candidate.group as u32),
                        symbols: vec![mana_color_code(color).to_string()],
                        restrictions: Vec::new(),
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
                    action_id: Some(interaction_action_id(&GameAction::CancelCast)),
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
                    action_id: Some(interaction_action_id(&GameAction::CancelCast)),
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
                    surfaces: vec![
                        InteractionPresentationSurface::Summary {
                            code: InteractionSummaryCode::Decision,
                        },
                        // CR 115.1: the opportunity-level label for what this
                        // announcement will do to whatever is chosen. The
                        // per-candidate identity surfaces are emitted by
                        // `target_sequence_choices`; what was missing here was
                        // the intent, so a consumer could not tell a kill spell
                        // from a pump spell.
                        InteractionPresentationSurface::Selection {
                            intent: projection.intent,
                            constraint: SelectionConstraint::Count { min, max },
                            confirm: ConfirmSemantics::Explicit,
                        },
                    ],
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
            // CR 732.2b: the proposal the responder is being asked to accept or shorten, decoded
            // from the SAME already-redacted waiting-for state the shorten window is read from.
            // Decoded HERE rather than carried on `ShortcutReplyProjection`: that struct is
            // `Copy` and is also read by `materialize_response` on every INBOUND reply, which
            // wants none of this.
            let (points, candidates, declared) =
                match declared_shortcut_projection(&filtered_state.waiting_for) {
                    Some(sequence) => (
                        loop_shortcut_points(&slot.interaction_id, &sequence.projection),
                        loop_shortcut_choices(
                            &slot.interaction_id,
                            &sequence.projection,
                            filtered_state,
                        ),
                        declared_sequence_preview(&slot.interaction_id, &sequence),
                    ),
                    None => (Vec::new(), Vec::new(), None),
                };
            // CR 601.2c: the element and the group of the announced-target decision it
            // partitions leave the producer as one value, so no reader has to infer either from
            // the other's presence.
            let (declared, allocation_group) = declared.unzip();
            (
                InteractionOpportunity {
                    interaction_id: slot.interaction_id.clone(),
                    response: InteractionOpportunityResponse::Schema {
                        spec: InteractionResponseSpec::ShortcutReply {
                            min_iteration: projection.min_iteration,
                            max_iteration: projection.max_iteration,
                            points,
                            declared,
                            allocation_group,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates,
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
                            min_main_total: projection.min_main_total,
                            max_main_total: projection.max_main_total,
                            confirm: ConfirmSemantics::Explicit,
                        },
                        candidates: sideboard_choices(&slot.interaction_id, &projection),
                    },
                    // `total` carries an *exact* required aggregate; CR 100.5
                    // leaves the main-deck size a range, so there is none to
                    // assert and `min`/`max` carry the whole constraint. Same
                    // encoding the range-valued shortcut surface uses.
                    surfaces: vec![InteractionPresentationSurface::Amount {
                        min: projection.min_main_total,
                        max: projection.max_main_total,
                        total: None,
                    }],
                    progress: InteractionProgress {
                        selected: 0,
                        minimum: projection.min_main_total,
                        maximum: Some(projection.max_main_total),
                        aggregate: Some(0),
                        confirmable: projection.min_main_total == 0,
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
            // AFTER the point projection, deliberately: the allocation speaks the very
            // choice ids that loop runs over.
            let preview = loop_shortcut_preview(&slot.interaction_id, &projection);
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
                            preview,
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
    // Membership follows visibility, so it is built before any authorization,
    // session or bounding gate and carried by every projection below. Overflow
    // is a value here, not a silently emptied map: the finalizer is the only
    // place allowed to decide what an unbounded projection becomes.
    let attachment_views = attachment_views_for_viewer(filtered_state);
    let authorized_submitters = interaction_authorized_submitters(authoritative_state);
    let can_submit = authorized_submitters.contains(&viewer);
    let kind = waiting_for_kind(&authoritative_state.waiting_for);
    if kind.terminal {
        return finalize_viewer_interaction(
            ViewerInteraction {
                waiting_for_kind: kind,
                authorized_submitters: Vec::new(),
                can_submit: false,
                auto_pass_recommended: false,
                opportunities: Vec::new(),
                attachment_fans: BTreeMap::new(),
                attachment_views: BTreeMap::new(),
                availability: InteractionAvailability::Terminal {
                    outcome: InteractionOutcomeCode::Terminal,
                },
            },
            attachment_views.clone(),
        );
    }
    if !can_submit {
        return finalize_viewer_interaction(
            ViewerInteraction {
                waiting_for_kind: kind,
                authorized_submitters: authorized_submitters
                    .into_iter()
                    .map(|player| player.0)
                    .collect(),
                can_submit: false,
                auto_pass_recommended: false,
                opportunities: Vec::new(),
                attachment_fans: BTreeMap::new(),
                attachment_views: BTreeMap::new(),
                availability: InteractionAvailability::Waiting,
            },
            attachment_views.clone(),
        );
    }
    if authoritative_state
        .interaction_session_id
        .as_ref()
        .is_none_or(|session| !interaction_session_is_valid(session))
    {
        return finalize_viewer_interaction(
            ViewerInteraction {
                waiting_for_kind: kind,
                authorized_submitters: authorized_submitters
                    .into_iter()
                    .map(|player| player.0)
                    .collect(),
                can_submit: true,
                auto_pass_recommended: false,
                opportunities: Vec::new(),
                attachment_fans: BTreeMap::new(),
                attachment_views: BTreeMap::new(),
                availability: InteractionAvailability::Unsupported {
                    reason: InteractionReasonCode::AuthorityUnbound,
                },
            },
            attachment_views.clone(),
        );
    }
    if !interaction_serial_is_valid(&authoritative_state.next_interaction_serial) {
        return finalize_viewer_interaction(
            ViewerInteraction {
                waiting_for_kind: kind,
                authorized_submitters: authorized_submitters
                    .into_iter()
                    .map(|player| player.0)
                    .collect(),
                can_submit: true,
                auto_pass_recommended: false,
                opportunities: Vec::new(),
                attachment_fans: BTreeMap::new(),
                attachment_views: BTreeMap::new(),
                availability: InteractionAvailability::Unsupported {
                    reason: InteractionReasonCode::InvalidAuthorityState,
                },
            },
            attachment_views.clone(),
        );
    }

    let slots: Vec<_> = authoritative_state
        .active_interaction_slots
        .iter()
        .filter(|slot| {
            interaction_submitter_for_owner(authoritative_state, PlayerId(slot.semantic_owner))
                == viewer
        })
        .collect();
    if slots.len() > MAX_INTERACTION_LIST_LEN {
        return finalize_viewer_interaction(
            ViewerInteraction {
                waiting_for_kind: kind,
                authorized_submitters: authorized_submitters
                    .into_iter()
                    .map(|player| player.0)
                    .collect(),
                can_submit: true,
                auto_pass_recommended: false,
                opportunities: Vec::new(),
                attachment_fans: BTreeMap::new(),
                attachment_views: BTreeMap::new(),
                availability: InteractionAvailability::Unsupported {
                    reason: InteractionReasonCode::PayloadTooLarge,
                },
            },
            attachment_views.clone(),
        );
    }
    let mut opportunities = Vec::with_capacity(slots.len());
    let mut attachment_fans = BTreeMap::new();
    let mut first_progress = None;
    let mut first_fallback = None;
    let default_availability = InteractionAvailability::Stuck {
        reason: InteractionReasonCode::NoLegalResponse,
    };
    for slot in slots {
        let (mut opportunity, mut slot_availability) =
            opportunity_for_slot(authoritative_state, filtered_state, viewer, slot);
        let opportunity_is_bounded = bound_outbound_opportunity(&opportunity).is_ok();
        if !opportunity_is_bounded {
            (opportunity, slot_availability) = payload_too_large_opportunity(&slot.interaction_id);
        }
        if opportunity_is_bounded
            && !matches!(
                slot_availability,
                InteractionAvailability::Unsupported { .. }
            )
        {
            attachment_fans.extend(attachment_fans_for_slot(
                authoritative_state,
                filtered_state,
                slot,
            ));
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
    let attachment_views = attachment_views.map(|mut views| {
        bind_attachment_view_submissions(&mut views, &attachment_fans);
        views
    });
    finalize_viewer_interaction(
        ViewerInteraction {
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
            attachment_fans,
            attachment_views: BTreeMap::new(),
            availability,
        },
        attachment_views,
    )
}

/// The single exit of [`derive_viewer_interaction`]: every projection the engine
/// hands outward — terminal, unauthorized, unbound-authority, invalid-serial,
/// oversized-slot and the derived one alike — leaves through here, so one
/// aggregate budget governs the whole payload. Membership is built before the
/// authorization and session gates, so an early return carries just as much of
/// it as the derived path does; charging it only on the derived path would leave
/// the other five able to serialize an unbounded attachment tree.
///
/// Failing the budget fails closed: the unbounded lists are dropped and the
/// availability states why, rather than shipping a truncated payload that reads
/// as an authoritative "nothing is attached".
///
/// `attachment_views` arrives as the projection or as the reason it could not be
/// produced within the bound, and is installed here rather than by the caller.
/// A membership map that overflowed inside its own derivation is therefore just
/// as visible to this gate as one that overflows the aggregate: both reach the
/// same fail-closed answer, and neither can leave as a plausible empty map.
fn finalize_viewer_interaction(
    mut view: ViewerInteraction,
    attachment_views: Result<BTreeMap<u64, InteractionAttachmentView>, InteractionReasonCode>,
) -> ViewerInteraction {
    let bounded = attachment_views.and_then(|views| {
        view.attachment_views = views;
        bound_outbound_view(&view)
    });
    if let Err(reason) = bounded {
        view.opportunities.clear();
        view.attachment_fans.clear();
        view.attachment_views.clear();
        view.availability = InteractionAvailability::Unsupported { reason };
    }
    view
}

/// Build attachment affordances from typed decision provenance. The host
/// back-link and child forward-link must agree; this avoids surfacing stale
/// relationship data or indirect descendants.
fn attachment_fans_for_slot(
    authoritative_state: &GameState,
    filtered_state: &GameState,
    slot: &ActiveInteractionSlot,
) -> BTreeMap<u64, InteractionAttachmentFan> {
    let semantic_owner = PlayerId(slot.semantic_owner);
    let model = human_response_model(&filtered_state.waiting_for, semantic_owner);
    let object_choices = match model {
        HumanResponseModel::TargetSequence => {
            target_sequence_projection(&filtered_state.waiting_for)
                .ok()
                .flatten()
                .into_iter()
                .flat_map(|projection| {
                    projection
                        .candidates
                        .into_iter()
                        .enumerate()
                        .filter_map(|(index, target)| match target {
                            TargetRef::Object(object_id) => Some((
                                object_id,
                                interaction_choice_id(&slot.interaction_id, 't', index),
                            )),
                            TargetRef::Player(_) => None,
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        }
        HumanResponseModel::Select => {
            selection_projection(&filtered_state.waiting_for, filtered_state, semantic_owner)
                .ok()
                .flatten()
                .into_iter()
                .flat_map(|projection| {
                    projection
                        .object_ids
                        .into_iter()
                        .enumerate()
                        .map(|(index, object_id)| {
                            (
                                object_id,
                                interaction_choice_id(&slot.interaction_id, 's', index),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        }
        HumanResponseModel::ExactCandidates(AuditedExactCandidates) => {
            actor_candidates(authoritative_state, semantic_owner)
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .filter_map(|(index, candidate)| {
                    candidate.action.source_object().map(|object_id| {
                        (
                            object_id,
                            interaction_choice_id(&slot.interaction_id, 'c', index),
                        )
                    })
                })
                .collect()
        }
        HumanResponseModel::Terminal
        | HumanResponseModel::AssignAmounts
        | HumanResponseModel::AmountAssignments
        | HumanResponseModel::DamageAssignments
        | HumanResponseModel::TriggerOrder
        | HumanResponseModel::CoinFlipSequence
        | HumanResponseModel::CategorySelection
        | HumanResponseModel::CombatRelations(_)
        | HumanResponseModel::ManaGroups(_)
        | HumanResponseModel::ModeSequence
        | HumanResponseModel::OutsideSelection
        | HumanResponseModel::TextChoice
        | HumanResponseModel::ShortcutReply
        | HumanResponseModel::DirectChoices
        | HumanResponseModel::SideboardPartition
        | HumanResponseModel::NumberRange(_)
        | HumanResponseModel::LoopShortcut => Vec::new(),
    };
    attachment_fans_for_object_choices(filtered_state, &slot.interaction_id, model, object_choices)
}

fn attachment_fans_for_object_choices(
    filtered_state: &GameState,
    interaction_id: &InteractionId,
    model: HumanResponseModel,
    object_choices: impl IntoIterator<Item = (ObjectId, InteractionChoiceId)>,
) -> BTreeMap<u64, InteractionAttachmentFan> {
    let mut fans: BTreeMap<
        (InteractionId, ObjectId),
        BTreeMap<ObjectId, Vec<InteractionChoiceId>>,
    > = BTreeMap::new();

    for (child_id, choice_id) in object_choices {
        let Some(child) = filtered_state.objects.get(&child_id) else {
            continue;
        };
        let Some(AttachTarget::Object(host_id)) = child.attached_to else {
            continue;
        };
        let Some(host) = filtered_state.objects.get(&host_id) else {
            continue;
        };
        if !host.attachments.contains(&child_id) {
            continue;
        }
        let choice_ids = fans
            .entry((interaction_id.clone(), host_id))
            .or_default()
            .entry(child_id)
            .or_default();
        if !choice_ids.contains(&choice_id) {
            choice_ids.push(choice_id);
        }
    }

    fans.into_iter()
        .filter_map(|((interaction_id, host_id), children)| {
            let children = children
                .into_iter()
                .filter_map(|(object_id, choice_ids)| {
                    let [choice_id] = choice_ids.as_slice() else {
                        return None;
                    };
                    attachment_fan_submission(&interaction_id, model, choice_id.clone()).map(
                        |submission| InteractionAttachmentFanChild {
                            object_id: object_id.0,
                            submission,
                        },
                    )
                })
                .collect::<Vec<_>>();
            (!children.is_empty()).then_some((
                host_id.0,
                InteractionAttachmentFan {
                    host_id: host_id.0,
                    children,
                },
            ))
        })
        .collect()
}

/// Only publish one-step attachment picks. Multi-choice objects and response
/// families that require the UI to synthesize a payload stay in the normal
/// interaction surface until the engine exposes a dedicated picker model.
fn attachment_fan_submission(
    interaction_id: &InteractionId,
    model: HumanResponseModel,
    choice_id: InteractionChoiceId,
) -> Option<InteractionSubmission> {
    let response = match model {
        HumanResponseModel::ExactCandidates(_) => InteractionResponse::Choose { choice_id },
        HumanResponseModel::Select => InteractionResponse::Select {
            choice_ids: vec![choice_id],
        },
        HumanResponseModel::TargetSequence => InteractionResponse::Sequence {
            choice_ids: vec![choice_id],
        },
        _ => return None,
    };
    Some(InteractionSubmission {
        interaction_id: interaction_id.clone(),
        response,
    })
}

/// Publish what is attached to every object the viewer can see.
///
/// Membership is a board fact — an attached permanent is an object in play
/// (CR 301.5 / CR 303.4), not a property of the current prompt — so this reads
/// the filtered projection alone and runs on every path, including the ones
/// that carry no opportunity: an opponent's turn, an open prompt, a finished
/// game. Publishing it only where picks exist would make an Aura disappear from
/// the one surface that shows it as soon as a sibling Equipment became
/// clickable.
///
/// Both directions of every relationship must agree, the same guard
/// [`attachment_fans_for_object_choices`] applies, so authority-only or stale
/// back-links cannot reach a consumer.
///
/// Overflow is returned, never absorbed. Dropping an oversized host or emptying
/// an oversized map here would hand the caller a bounded, plausible projection
/// that states the opposite of the truth — "nothing is attached" — and the
/// budget gate downstream would have nothing left to object to. The reason code
/// travels instead, and [`finalize_viewer_interaction`] decides what the viewer
/// is told.
fn attachment_views_for_viewer(
    filtered_state: &GameState,
) -> Result<BTreeMap<u64, InteractionAttachmentView>, InteractionReasonCode> {
    let mut views = BTreeMap::new();
    // Cards materialized so far, across every host. Charged BEFORE each subtree
    // is built rather than measured after, so an over-budget projection is
    // refused while it is still small.
    //
    // Every card the derivation emits is charged again by
    // `bound_outbound_view`, alongside the map, the fans and the opportunities —
    // so this running total can only ever be a LOWER bound on what the finalizer
    // charges. That direction is what makes the early refusal safe: it can never
    // reject a projection the finalizer would have accepted, and the finalizer
    // stays the single authority on the answer the viewer is given.
    let mut charged = 0usize;
    for host_id in filtered_state.objects.keys().copied() {
        let mut cards = Vec::new();
        collect_attachment_subtree(filtered_state, host_id, charged, &mut cards)?;
        if cards.is_empty() {
            continue;
        }
        charged += cards.len();
        views.insert(
            host_id.0,
            InteractionAttachmentView {
                host_id: host_id.0,
                cards: cards
                    .into_iter()
                    .map(|object_id| InteractionAttachmentViewCard {
                        object_id: object_id.0,
                        submission: None,
                    })
                    .collect(),
            },
        );
    }
    // Implied by the card charge — no view is inserted empty, so the map can
    // never be longer than the cards already counted — but stated where the map
    // is built so its own list bound does not have to be inferred.
    if views.len() > MAX_INTERACTION_LIST_LEN {
        return Err(InteractionReasonCode::PayloadTooLarge);
    }
    Ok(views)
}

/// Depth-first pre-order, so a nested attachment follows the card it hangs on
/// and consumers can lay the subtree out without re-deriving its shape.
///
/// Iterative, with the frontier on the heap. The walk descends one level per
/// nested attachment, and an attachment chain is only bounded by how many
/// objects the game can grow, so recursion would put a game-controlled quantity
/// on the call stack.
///
/// `already_charged` is what earlier hosts have spent, and the walk stops the
/// moment this subtree carries the running total past the aggregate. An
/// over-budget projection therefore costs one card more than the budget rather
/// than its full size: the point of deriving membership is to publish it, and a
/// projection that can no longer be published is not worth materializing.
///
/// The running total is compared by addition rather than by handing the walk a
/// remaining allowance, so the bound cannot be expressed as a subtraction that
/// underflows if the caller's accounting ever slips.
fn collect_attachment_subtree(
    filtered_state: &GameState,
    host_id: ObjectId,
    already_charged: usize,
    out: &mut Vec<ObjectId>,
) -> Result<(), InteractionReasonCode> {
    // Seeded with the host: an attachment cycle would otherwise walk forever,
    // and no host may appear as a card beneath itself.
    let mut seen = HashSet::from([host_id]);
    let mut frontier = vec![host_id];
    while let Some(current) = frontier.pop() {
        if current != host_id {
            out.push(current);
            if already_charged + out.len() > MAX_INTERACTION_LIST_LEN {
                return Err(InteractionReasonCode::PayloadTooLarge);
            }
        }
        let Some(object) = filtered_state.objects.get(&current) else {
            continue;
        };
        // Pushed in reverse so the frontier pops them in the order the host
        // lists them, which is what keeps the output in pre-order.
        for child_id in object.attachments.iter().rev() {
            let Some(child) = filtered_state.objects.get(child_id) else {
                continue;
            };
            if !matches!(child.attached_to, Some(AttachTarget::Object(id)) if id == current) {
                continue;
            }
            if !seen.insert(*child_id) {
                continue;
            }
            frontier.push(*child_id);
        }
    }
    Ok(())
}

/// Bind the published picks onto the membership the engine already owns.
///
/// The fans are keyed by the child's DIRECT host, so a card published beneath a
/// nested attachment must still reach the view of the outermost host it hangs
/// under. Object ids are unique, so a flat index is exact: it cannot move a
/// pick onto a different card, and every host that legitimately lists the card
/// gets the same submission.
fn bind_attachment_view_submissions(
    views: &mut BTreeMap<u64, InteractionAttachmentView>,
    fans: &BTreeMap<u64, InteractionAttachmentFan>,
) {
    if fans.is_empty() {
        return;
    }
    let submissions: HashMap<u64, &InteractionSubmission> = fans
        .values()
        .flat_map(|fan| fan.children.iter())
        .map(|child| (child.object_id, &child.submission))
        .collect();
    for view in views.values_mut() {
        for card in &mut view.cards {
            card.submission = submissions.get(&card.object_id).map(|s| (*s).clone());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionSubmitError {
    pub code: InteractionReasonCode,
}

/// Successful interaction submission. The action is the exact opaque response
/// materialized by the engine and is available to trusted adapters solely for
/// post-success replay recording.
#[derive(Debug, Clone)]
pub struct AppliedInteraction {
    pub action: GameAction,
    pub result: ActionResult,
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
        InteractionPresentationSurface::Mana {
            symbols,
            restrictions,
            ..
        } => {
            budget.list(symbols.len())?;
            for symbol in symbols {
                budget.string(symbol)?;
            }
            budget.list(restrictions.len())?;
            for restriction in restrictions {
                bound_outbound_mana_restriction(restriction, budget)?;
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

fn bound_outbound_mana_restriction(
    restriction: &InteractionManaRestriction,
    budget: &mut OutboundBudget,
) -> Result<(), InteractionReasonCode> {
    match restriction {
        InteractionManaRestriction::OnlyForSpell
        | InteractionManaRestriction::OnlyForActivation
        | InteractionManaRestriction::OnlyForXCosts
        | InteractionManaRestriction::OnlyForFaceDownSpell
        | InteractionManaRestriction::Impossible
        | InteractionManaRestriction::ConvokePayment => {}
        InteractionManaRestriction::OnlyForSpellType { spell_type }
        | InteractionManaRestriction::OnlyForCreatureType {
            creature_type: spell_type,
        }
        | InteractionManaRestriction::OnlyForTaggedActivation { tag: spell_type }
        | InteractionManaRestriction::OnlyForSpellWithKeywordKind {
            keyword: spell_type,
        } => {
            budget.string(spell_type)?;
        }
        InteractionManaRestriction::OnlyForTypeSpellsOrAbilities { spell_type, .. } => {
            budget.string(spell_type)?;
        }
        InteractionManaRestriction::OnlyForSpellWithKeywordKindFromZone { keyword, .. } => {
            budget.string(keyword)?;
        }
        InteractionManaRestriction::OnlyForSpellMatchingCostCriteria {
            spell_type,
            criteria,
        } => {
            if let Some(spell_type) = spell_type {
                budget.string(spell_type)?;
            }
            budget.list(criteria.len())?;
        }
        InteractionManaRestriction::OnlyForSpellWithManaValue { .. }
        | InteractionManaRestriction::OnlyForSpellWithColorCount { .. }
        | InteractionManaRestriction::OnlyForSpellColor { .. }
        | InteractionManaRestriction::OnlyForSpellFromZone { .. }
        | InteractionManaRestriction::CannotCastSpellFromZone { .. }
        | InteractionManaRestriction::OnlyForSpecialAction { .. } => {}
        InteractionManaRestriction::OnlyForAny { restrictions } => {
            budget.list(restrictions.len())?;
            for restriction in restrictions {
                bound_outbound_mana_restriction(restriction, budget)?;
            }
        }
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
        InteractionResponseSpec::Shortcut {
            points, preview, ..
        } => {
            budget.list(points.len())?;
            for point in points {
                budget.list(point.candidate_ids.len())?;
                for candidate_id in &point.candidate_ids {
                    budget.string(candidate_id.as_str())?;
                }
            }
            // The preview is a published outbound list of lists — one element per sampled
            // count, each carrying its own entries and allocation — so every level is charged
            // to the same ceiling rather than crossing uncounted.
            budget.list(preview.len())?;
            for element in preview {
                budget.list(element.entries.len())?;
                budget.list(element.allocation.len())?;
            }
        }
        // CR 732.2b: the responder's copy of the declaration is the same shape of outbound list
        // of lists the offer publishes, so it is charged on the same cumulative budget rather
        // than crossing uncounted.
        InteractionResponseSpec::ShortcutReply {
            points, declared, ..
        } => {
            budget.list(points.len())?;
            for point in points {
                budget.list(point.candidate_ids.len())?;
                for candidate_id in &point.candidate_ids {
                    budget.string(candidate_id.as_str())?;
                }
            }
            if let Some(element) = declared {
                budget.list(element.entries.len())?;
                budget.list(element.allocation.len())?;
            }
        }
        InteractionResponseSpec::Select { .. }
        | InteractionResponseSpec::AssignAmounts { .. }
        | InteractionResponseSpec::Text { .. }
        | InteractionResponseSpec::DeckPartition { .. }
        | InteractionResponseSpec::Number { .. } => {}
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
                // Charged on the SAME cumulative ceiling the `choice_ids` legs charge, so the
                // two walks over one struct cannot disagree about what is authoritative.
                budget.list(pin.amounts.len())?;
                for assignment in &pin.amounts {
                    budget.string(assignment.choice_id.as_str())?;
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
    budget.list(view.attachment_fans.len())?;
    for fan in view.attachment_fans.values() {
        budget.list(fan.children.len())?;
        for child in &fan.children {
            budget.string(child.submission.interaction_id.as_str())?;
            bound_outbound_response(&child.submission.response, &mut budget)?;
        }
    }
    budget.list(view.attachment_views.len())?;
    for attachment_view in view.attachment_views.values() {
        budget.list(attachment_view.cards.len())?;
        for card in &attachment_view.cards {
            let Some(submission) = &card.submission else {
                continue;
            };
            budget.string(submission.interaction_id.as_str())?;
            bound_outbound_response(&submission.response, &mut budget)?;
        }
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
                // A client-controlled list of client-controlled strings at a trust boundary,
                // charged to the SAME cumulative ceiling the `choice_ids` legs charge — so a
                // submission whose per-pin lists are each legal but whose sum is not is refused
                // here, where the engine is the sole authority for these bounds.
                budget.list(pin.amounts.len())?;
                for assignment in &pin.amounts {
                    budget.string(assignment.choice_id.as_str())?;
                }
            }
            Ok(())
        }
        InteractionResponse::Number { .. } | InteractionResponse::ShortcutReply { .. } => Ok(()),
    }
}

/// Wire-boundary bounds for one inbound submission, evaluated without touching
/// game state.
///
/// Transports call this at the wire so a rejection is answered before the
/// dispatcher does identity work; [`submit_interaction`] re-runs it via
/// [`resolve_interaction_response`], so no caller can skip it and no transport
/// can drift from these bounds by restating them.
pub fn bound_interaction_submission(
    submission: &InteractionSubmission,
) -> Result<(), InteractionSubmitError> {
    bound_string(submission.interaction_id.as_str())?;
    validate_response_bounds(&submission.response)?;
    Ok(())
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
    let authorized = interaction_submitter_for_owner(state, PlayerId(slot.semantic_owner));
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
    // CR 100.2a / CR 100.4a / CR 100.5: the main deck must land inside the
    // projected interval, not on an exact size — a larger main deck is legal
    // as long as the sideboard still fits under its cap.
    let Some(main_total) = main_counts
        .iter()
        .try_fold(0u32, |total, count| total.checked_add(*count))
    else {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    };
    if main_total < projection.min_main_total || main_total > projection.max_main_total {
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
            minimum: projection.min_main_total,
            maximum: Some(projection.max_main_total),
            aggregate: i32::try_from(main_total).ok(),
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
        // CR 732.2a: a SEQUENCED pin answers ONE target position with an ordered announcement
        // sequence, so its `choice_ids` may exceed the point's `max`. Every other pin decodes
        // exactly as before.
        let sequenced = !pin.amounts.is_empty() || pin.choice_ids.len() > point.max as usize;
        // CR 732.2a: a SEQUENCED pin is ONE target position's partition of a DECLARED count, and
        // nothing else. A multi-position slot needs a per-position carrier a flat list cannot
        // express; an until-lethal proposal has no count to partition, so a sequence there names
        // announcements past the head that no drive ever reads (CR 732.2c). Both are refused
        // rather than mis-read. Binding the count here carries "this can only be a `Fixed`
        // partition" in the type instead of in a comment.
        let sequenced_partition: Option<u32> = if sequenced {
            match (&count, &point.kind) {
                (IterationCount::Fixed(declared), InteractionShortcutPointKind::Targets)
                    if point.max == 1 =>
                {
                    Some(*declared)
                }
                _ => return Err(InteractionReasonCode::ConstraintUnsatisfied),
            }
        } else {
            None
        };
        if pin.choice_ids.len() < point.min as usize
            || (!sequenced && pin.choice_ids.len() > point.max as usize)
            || (point.unique
                && pin.choice_ids.iter().collect::<HashSet<_>>().len() != pin.choice_ids.len())
        {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        // CR 732.2a: `InteractionProgress.selected` counts POSITIONS ANSWERED. A sequenced pin
        // answers its point's positions, not one per subject in the sequence; charging the
        // sequence length would publish `selected > maximum`. On a flat pin the `.min` is the
        // identity, because the guard above already bounds the length by `point.max`.
        selected = selected
            .checked_add((pin.choice_ids.len() as u32).min(point.max))
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
                let subjects = candidate_indices
                    .iter()
                    .map(|index| {
                        shortcut_announcement_subject(
                            &projection.candidates[*index],
                            authoritative_state,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let targets = if let Some(declared) = sequenced_partition {
                    vec![decode_sequenced_targets(declared, pin, subjects)?]
                } else {
                    // CR 400.7 vs CR 601.2c: an OBJECT position keeps the identity spelling it
                    // already had, re-bound through `decision_template::resolve_source`
                    // (battlefield-only). Any other subject answers its position through the
                    // one-entry `Ranking`, where `evaluate_schedule` judges it by
                    // `targeting::player_is_legal_target` rather than by existence alone. The
                    // ranked arm BINDS rather than names its variant: `evaluate_schedule`,
                    // `game::visibility` and `types::actions` each match `AnnouncementSubject`
                    // exhaustively, so a future subject variant still build-breaks where the
                    // decision belongs — at the resolver, not at this ingress.
                    subjects
                        .into_iter()
                        .map(|subject| match subject {
                            AnnouncementSubject::Object(source) => TargetPin::ByIdentity(source),
                            ranked => {
                                TargetPin::Scheduled(TargetSchedule::Constant(Ranking::one(ranked)))
                            }
                        })
                        .collect()
                };
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
                        LoopShortcutCandidateValue::Target(TargetRef::Player(_))
                        | LoopShortcutCandidateValue::Target(TargetRef::Object(_))
                        | LoopShortcutCandidateValue::ConvokeObject(_)
                        | LoopShortcutCandidateValue::May(_)
                        | LoopShortcutCandidateValue::Unless(_)
                        | LoopShortcutCandidateValue::ManaColor(_) => {
                            Err(InteractionReasonCode::InvalidAuthorityState)
                        }
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
        // CR 732.2a: validate over the range the ACCEPTED COUNT will drive. This decoder now
        // emits `TargetSchedule::Piecewise`, whose value at an index is not its value at index
        // 0, so an index-0-only check would accept a declaration whose driven image leaves the
        // offer's published legal set at an index the count reaches. The helper's precondition
        // — a count already bounded — is discharged here by the count-spec projection, which
        // computes `max = schema.max_iterations.min(MAX_SHORTCUT_CYCLES)` and admits only that
        // window.
        //
        // The `required` slot list is still not derived here: `declaration_conforms` derives it
        // from the SAME `authoritative_schema` this site already passed.
        if !declaration_conforms(
            authoritative_schema,
            template,
            crate::game::engine::shortcut_validated_range(&count, Some(template)),
            authoritative_state,
        ) {
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

/// CR 601.2c: the announcement subject one `Targets`-point candidate names.
///
/// THE ONE PLACE THIS MODULE NAMES AN ANNOUNCEMENT SUBJECT. The per-position pin and the
/// sequenced pin both speak through it, so one announcement cannot acquire two spellings
/// depending on which shape carried it. A candidate on a `Targets` point is an announced
/// TARGET, so a seat takes the TARGET class — judged by `targeting::player_is_legal_target`
/// (CR 702.11c hexproof / CR 702.18a shroud / CR 702.16b protection) — and never the
/// CR 115.10a CHOICE class `TargetPin::Player`, whose authority is existence alone. Emitting
/// that spelling here would select the authority by WHO SUBMITTED the answer rather than by
/// WHAT IT IS.
fn shortcut_announcement_subject(
    candidate: &LoopShortcutCandidateValue,
    state: &GameState,
) -> Result<AnnouncementSubject, InteractionReasonCode> {
    match candidate {
        LoopShortcutCandidateValue::Target(TargetRef::Player(player)) => {
            Ok(AnnouncementSubject::Seat(*player))
        }
        LoopShortcutCandidateValue::Target(TargetRef::Object(object_id)) => {
            let object = state
                .objects
                .get(object_id)
                .ok_or(InteractionReasonCode::ConstraintUnsatisfied)?;
            // CR 400.7: bind the submitted target to this object's current incarnation so a
            // zone change cannot silently retarget the replay.
            Ok(AnnouncementSubject::Object(
                crate::types::game_state::YieldTarget::ThisObject {
                    source_id: *object_id,
                    incarnation: Some(object.incarnation),
                    trigger_description: None,
                },
            ))
        }
        LoopShortcutCandidateValue::ConvokeObject(_)
        | LoopShortcutCandidateValue::Mode(_)
        | LoopShortcutCandidateValue::May(_)
        | LoopShortcutCandidateValue::Unless(_)
        | LoopShortcutCandidateValue::ManaColor(_) => {
            Err(InteractionReasonCode::InvalidAuthorityState)
        }
    }
}

/// CR 732.2a + CR 601.2c: decode ONE SEQUENCED `Targets` pin — a DECLARED count's partition
/// across an ordered announcement sequence for a SINGLE target position — into the one
/// `TargetPin` it names.
///
/// ONE MODE. `declared` is the count to partition: `amounts` partitions the sequence
/// one-for-one and in order, every part at least 1, summing to `declared`. Segment starts are
/// the running prefix sums, so the pin is a `TargetSchedule::Piecewise`. Positive parts summing
/// to `declared` put every segment start inside `0..declared`, which is the range
/// `game::engine::shortcut_validated_range` validates, so this producer can never mint a
/// `ScheduleExhausted`. An until-lethal proposal names no count to partition and is not
/// sequenced at all — its caller refuses the shape before reaching here, and its submission
/// decodes through the ordinary flat `Targets` arm.
///
/// `Ranking::new` validates the sequence, and it is the only thing that can:
/// `loop_shortcut_projection` hard-codes `unique: false` into every `Targets` point it mints,
/// so `point.unique` cannot refuse a duplicate for any member of this class. Here the
/// duplicate-free clause is this ingress's deliberate restriction rather than a rule
/// consequence — two disjoint `Piecewise` segments naming one seat are two announcements at two
/// iterations, and the engine accepts that shape.
/// The admitted set is compositions of the declared count into positive parts over a
/// DUPLICATE-FREE subset of the published candidates. Non-contiguous allocations are foreclosed
/// here, and an authoring surface built on this ingress inherits the foreclosure.
///
/// The result is ONE `TargetPin`, so `declaration_conforms`' `targets.len()` window needs no
/// relaxation and none is made.
fn decode_sequenced_targets(
    declared: u32,
    pin: &InteractionShortcutPin,
    subjects: Vec<AnnouncementSubject>,
) -> Result<TargetPin, InteractionReasonCode> {
    let sequence =
        Ranking::new(subjects).map_err(|_| InteractionReasonCode::ConstraintUnsatisfied)?;
    // One amount per announced subject, in the sequence's own order. This conjunct also refuses
    // the amount-free sequence: `choice_ids` longer than the point's `max` with no declared
    // lengths cannot partition a finite count.
    if pin.amounts.len() != pin.choice_ids.len() {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    let mut start = 0u32;
    let mut segments = Vec::with_capacity(pin.amounts.len());
    for (assignment, (choice_id, subject)) in pin
        .amounts
        .iter()
        .zip(pin.choice_ids.iter().zip(sequence.iter()))
    {
        // A composition's parts are POSITIVE: a zero is not a part of the declared count, and
        // it would also collide two segment starts.
        if assignment.choice_id != *choice_id || assignment.amount == 0 {
            return Err(InteractionReasonCode::ConstraintUnsatisfied);
        }
        segments.push((start, Ranking::one(subject.clone())));
        start = start
            .checked_add(assignment.amount)
            .ok_or(InteractionReasonCode::PayloadTooLarge)?;
    }
    if start != declared {
        return Err(InteractionReasonCode::ConstraintUnsatisfied);
    }
    Ok(TargetPin::Scheduled(TargetSchedule::Piecewise(segments)))
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
        shortcut_preview: None,
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
            shortcut_preview: declared_shortcut_preview(
                &filtered.waiting_for,
                &request.interaction_id,
                &request.response,
            ),
        },
        Err(_) => rejected(InteractionReasonCode::ReducerRejected, progress),
    }
}

/// Preview an opaque interaction while returning the same safe rejection DTO
/// used by ordinary actions. Failures before an interaction response has been
/// materialized intentionally carry no object ids; the capability itself is
/// opaque and must not reveal the rejected opportunity's contents.
pub fn preview_interaction_with_rejection(
    state: &GameState,
    actor: PlayerId,
    request: &InteractionPreviewRequest,
) -> Result<InteractionPreview, ActionRejection> {
    bound_string(request.request_id.as_str())
        .and_then(|_| bound_string(request.interaction_id.as_str()))
        .and_then(|_| validate_response_bounds(&request.response))
        .map_err(action_rejection_for_interaction_reason)?;
    let semantic_owner = slot_for_submission(state, actor, &request.interaction_id)
        .map_err(action_rejection_for_interaction_reason)?
        .semantic_owner;
    let filtered = visibility::filter_state_for_viewer(state, actor);
    let (action, progress) =
        materialize_response(state, &filtered, &request.interaction_id, &request.response)
            .map_err(action_rejection_for_interaction_reason)?;
    let related_object_ids = action.related_object_ids();
    let mut projected = state.clone();
    apply_interaction_for_simulation(&mut projected, actor, PlayerId(semantic_owner), action)
        .map_err(|error| {
            visibility::filter_action_rejection_for_viewer(
                state,
                actor,
                &action_rejection_for_engine_error(&error, related_object_ids),
            )
        })?;
    Ok(InteractionPreview {
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
        shortcut_preview: declared_shortcut_preview(
            &filtered.waiting_for,
            &request.interaction_id,
            &request.response,
        ),
    })
}

/// Materialize the `GameAction` an interaction response denotes, **without**
/// applying it.
///
/// [`submit_interaction`] is the mutating path and delegates here for everything
/// up to the reducer, so the two cannot drift. This variant exists for consumers
/// that own their own dispatch and need the action itself — the ManaBrew adapter
/// translates a client's prompt answer into a `GameAction` and returns it to its
/// caller rather than applying it.
///
/// Such a consumer must never re-derive this mapping. `materialize_response`
/// matches exhaustively on `HumanResponseModel` with no catch-all arm, so the
/// compiler forces every new decision family through it; a reimplementation
/// living outside the engine would keep compiling while silently going stale.
///
/// Dropping the mutation does not weaken authorization. `slot_for_submission`
/// still authenticates `actor` against the slot, so this cannot be used to
/// materialize a decision that belongs to another player.
pub fn resolve_interaction_response(
    state: &GameState,
    actor: PlayerId,
    submission: &InteractionSubmission,
) -> Result<GameAction, InteractionSubmitError> {
    bound_interaction_submission(submission)?;
    slot_for_submission(state, actor, &submission.interaction_id)?;
    let filtered = visibility::filter_state_for_viewer(state, actor);
    let (action, _) = materialize_response(
        state,
        &filtered,
        &submission.interaction_id,
        &submission.response,
    )?;
    Ok(action)
}

/// Hidden engine-only submission entry point. The opaque interaction and choice
/// IDs are looked up against current trusted state, authorization is rechecked,
/// projection is recomputed from a viewer-filtered clone, and the materialized
/// action enters the same actor guard/reducer/outward boundary as legacy actions.
pub fn submit_interaction(
    state: &mut GameState,
    actor: PlayerId,
    submission: InteractionSubmission,
) -> Result<AppliedInteraction, InteractionSubmitError> {
    let action = resolve_interaction_response(state, actor, &submission)?;
    // Re-read the slot rather than threading it out of `resolve_*`: keeping that
    // function's return to the action alone is what makes it usable as a public
    // seam. The lookup is a scan of `active_interaction_slots`, which holds one
    // slot per pending decision, and it has already succeeded once here.
    let semantic_owner =
        PlayerId(slot_for_submission(state, actor, &submission.interaction_id)?.semantic_owner);
    let result = apply_interaction(state, actor, semantic_owner, action.clone()).map_err(
        |_error: EngineError| InteractionSubmitError {
            code: InteractionReasonCode::ReducerRejected,
        },
    )?;
    Ok(AppliedInteraction { action, result })
}

/// Submits an opaque interaction with stable rejection metadata. The legacy
/// [`submit_interaction`] result remains unchanged for existing transports.
pub fn submit_interaction_with_rejection(
    state: &mut GameState,
    actor: PlayerId,
    submission: InteractionSubmission,
) -> Result<AppliedInteraction, ActionRejection> {
    let action = resolve_interaction_response(state, actor, &submission)
        .map_err(|error| action_rejection_for_interaction_reason(error.code))?;
    let semantic_owner = PlayerId(
        slot_for_submission(state, actor, &submission.interaction_id)
            .map_err(action_rejection_for_interaction_reason)?
            .semantic_owner,
    );
    let result = apply_interaction_with_rejection(state, actor, semantic_owner, action.clone())?;
    Ok(AppliedInteraction { action, result })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cannot_cast_from_zone_projection_is_lossless() {
        let projection =
            interaction_mana_restriction(&ManaRestriction::CannotCastSpellFromZone(Zone::Hand));
        assert_eq!(
            projection,
            InteractionManaRestriction::CannotCastSpellFromZone {
                zone: InteractionZoneCode::Hand,
            }
        );
        assert_eq!(
            serde_json::to_value(&projection).unwrap(),
            serde_json::json!({
                "type": "cannotCastSpellFromZone",
                "data": { "zone": "hand" }
            })
        );
    }

    #[test]
    fn choose_objects_projection_preserves_runtime_cardinality() {
        let waiting = WaitingFor::ChooseObjectsSelection {
            player: PlayerId(0),
            eligible: vec![
                TargetRef::Object(ObjectId(1)),
                TargetRef::Object(ObjectId(2)),
                TargetRef::Object(ObjectId(3)),
            ],
            min: 1,
            max: Some(4),
            trigger_event: None,
        };
        let projection = target_sequence_projection(&waiting)
            .expect("projection must succeed")
            .expect("choose-objects is a target sequence");
        assert_eq!((projection.min, projection.max), (1, 3));
        assert!(projection.unique);
    }

    /// F4 — the preview is budgeted like every other outbound list on the shortcut spec, at
    /// all THREE of the levels it now has: the count-keyed element list, each element's
    /// entries, and each element's allocation.
    ///
    /// Each leg is sized so that exactly ONE of the three charges is what tips the budget over,
    /// which is what separates them: the element list against a budget that fits every entry,
    /// and the allocation against a budget that fits the element list and every entry.
    ///
    /// WHAT WRONG IMPLEMENTATION WOULD STILL PASS THIS ROW? One that budgets these three lists
    /// but not a future fourth added to the same spec — the row pins the fields it names, not
    /// "every field is budgeted". One that charged any of them to the STRING budget instead
    /// would fail here, because the controls prove the LIST budget is what moved.
    ///
    /// REVERT-PROBES: drop `budget.list(preview.len())` ⇒ the element-list leg gets `Ok`; drop
    /// `budget.list(element.allocation.len())` ⇒ the allocation leg gets `Ok`.
    #[test]
    fn the_shortcut_preview_lists_are_counted_against_the_outbound_budget() {
        let spec = |preview| InteractionResponseSpec::Shortcut {
            count: InteractionShortcutCountSpec::Fixed {
                min: 1,
                max: 3,
                suggested: 3,
            },
            points: Vec::new(),
            allow_decline: true,
            preview,
            confirm: ConfirmSemantics::Explicit,
        };
        let element = |count: u32, allocation: Vec<AmountAssignment>| InteractionShortcutPreview {
            count,
            entries: vec![
                InteractionShortcutPreviewEntry {
                    family: InteractionShortcutPreviewFamily::Life,
                    player: Some(1),
                    amount: -6,
                },
                InteractionShortcutPreviewEntry {
                    family: InteractionShortcutPreviewFamily::Mana,
                    player: None,
                    amount: 9,
                },
            ],
            allocation,
        };
        let counts = || 1..=u32::try_from(MAX_SHORTCUT_PREVIEW_ELEMENTS).unwrap();
        let plain: Vec<_> = counts().map(|count| element(count, Vec::new())).collect();
        let allocated: Vec<_> = counts()
            .map(|count| {
                element(
                    count,
                    vec![AmountAssignment {
                        choice_id: InteractionChoiceId("k0".to_string()),
                        amount: count,
                    }],
                )
            })
            .collect();
        let entry_slots: usize = plain.iter().map(|element| element.entries.len()).sum();
        let allocation_slots: usize = allocated
            .iter()
            .map(|element| element.allocation.len())
            .sum();
        assert!(
            allocation_slots > 0 && entry_slots > 0,
            "reach-guard: both staged lists must actually hold entries, or the budgets derived \
             from them below refuse for want of a list rather than for want of a charge"
        );
        let at_free_slots = |free: usize| OutboundBudget {
            entries: MAX_INTERACTION_LIST_LEN - free,
            string_bytes: 0,
        };

        // ── CONTROL FIRST: an empty preview list leaves this spec fitting in one slot, so
        //    every refusal below is a preview charge and not the spec being oversized.
        let mut budget = at_free_slots(1);
        assert!(
            bound_outbound_spec(&spec(Vec::new()), &mut budget).is_ok(),
            "control: with one slot free and no published element, this spec's own lists fit"
        );

        // ── THE ELEMENT LIST: room for every entry, none for the elements carrying them.
        let mut budget = at_free_slots(entry_slots);
        assert_eq!(
            bound_outbound_spec(&spec(plain.clone()), &mut budget),
            Err(InteractionReasonCode::PayloadTooLarge),
            "CR 732.2a: the count-keyed element list is published outbound and charged, so a \
             budget sized to its entries alone refuses it"
        );

        // ── THE ALLOCATION, against its own paired control on the same budget.
        let mut budget = at_free_slots(plain.len() + entry_slots);
        assert!(
            bound_outbound_spec(&spec(plain), &mut budget).is_ok(),
            "control: the element list and its entries fit exactly here, so the refusal below \
             is the allocation and nothing else"
        );
        let mut budget = at_free_slots(allocated.len() + entry_slots);
        assert_eq!(
            bound_outbound_spec(&spec(allocated), &mut budget),
            Err(InteractionReasonCode::PayloadTooLarge),
            "CR 601.2c: each element's allocation is a third published list and is charged \
             separately from its entries"
        );
    }

    /// F5 — the offer channel and the HUD channel must SPELL each display family identically.
    ///
    /// `InteractionShortcutPreviewFamily` is `rename_all = "camelCase"`; its grouping authority
    /// `derived_views::UnboundedFamily` is `rename_all = "lowercase"`. All eleven variants are
    /// single words today, so both spell `mana`, `life`, ... and the agreement reads as design
    /// when it is coincidence. A future two-word family would cross as `extraTurns` on the
    /// offer and `extraturns` on the HUD — one grouping published in two wire vocabularies,
    /// and THIS row is what catches it. Nothing on the client does: no client code reads the
    /// preview's `family` today (the generated `InteractionShortcutPreviewFamily` in
    /// `client/src/adapter/generated/interaction/index.ts` has no consumer), and the HUD's
    /// family-keyed lookups (`UNBOUNDED_FAMILY_GLYPH` / `UNBOUNDED_FAMILY_LABEL_KEY`, both
    /// `Record<UnboundedFamily, _>` in `client/src/components/hud/HudBadges.tsx`) are keyed by
    /// the SEPARATELY declared hand-written `UnboundedFamily` union — so a future consumer that
    /// crossed the two would break as a TypeScript type error, not miss silently.
    ///
    /// `preview_family`'s exhaustive match pins the GROUPING, not the STRING — a new family
    /// build-breaks it, a renamed WIRE STRING does not. This row pins the string.
    ///
    /// It takes its family list from `unbounded-family-tags.json`, the same golden the client's
    /// `Record<ResourceAxisTag, UnboundedFamily>` is checked against, so one chain now runs
    /// engine grouping ⇒ HUD string ⇒ offer string. That also makes the list forced rather than
    /// hand-maintained: an 18th `ResourceAxis` reds `family_tag_table_matches_the_client_golden`
    /// until the golden is regenerated, and a regenerated golden carries the new family here.
    ///
    /// THIS ROW PASSES TODAY BY CONSTRUCTION, AND THAT IS THE POINT — it is written to fail on
    /// a two-word variant, which is the only way the divergence can ship.
    ///
    /// WHAT WRONG IMPLEMENTATION WOULD STILL PASS THIS ROW? One that mis-GROUPS an axis (that
    /// is `family_tag_table_matches_the_client_golden`'s question, not this one), and one that
    /// adds a family reachable from no `ResourceAxis` at all, which the golden cannot see and
    /// no client lookup can receive.
    ///
    /// REVERT-PROBE, RUN: rename the `Turns` variant of BOTH enums to `ExtraTurns` and
    /// regenerate the golden ⇒ `extraturns` vs `extraTurns` ⇒ this row FAILS.
    #[test]
    fn every_preview_family_spells_the_same_wire_string_as_its_unbounded_family() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../client/src/test/fixtures/unbounded-family-tags.json"
        );
        let golden: BTreeMap<String, UnboundedFamily> =
            serde_json::from_str(&std::fs::read_to_string(path).expect("committed family golden"))
                .expect("the family golden parses as tag -> UnboundedFamily");
        let families: std::collections::BTreeSet<UnboundedFamily> =
            golden.values().copied().collect();
        assert_eq!(
            families.len(),
            11,
            "reach-guard: every display family must be reachable from the golden, else this \
             row silently checks a subset of the wire surface"
        );

        for family in families {
            let hud = serde_json::to_string(&family).expect("UnboundedFamily serializes");
            let offer =
                serde_json::to_string(&preview_family(family)).expect("preview family serializes");
            assert_eq!(
                hud, offer,
                "the shortcut offer and the HUD badge must cross the wire under the SAME \
                 string for this display family — the client keys both into one \
                 `Record<UnboundedFamily, _>`, so a divergence is a silent lookup miss"
            );
        }
    }

    /// The derivation itself, not the projection it feeds.
    ///
    /// `attachment_views_for_viewer` is the authority that decides whether the
    /// membership can be published at all, and the public rows in
    /// `interaction_contract` cannot see the difference this change makes: the
    /// aggregate bound in `bound_outbound_view` already refused this payload, so
    /// the ANSWER was fail-closed before and after. What changes is that the
    /// refusal now happens while the projection is small.
    ///
    /// A chain of exactly `MAX_INTERACTION_LIST_LEN` links is the worst case and
    /// the only one that discriminates: it is the LONGEST chain in which no
    /// single host's subtree exceeds the per-view cap, so the per-host check
    /// never fires and a derivation that measures afterwards runs to completion.
    /// One link longer and the outermost view trips that cap on its own.
    ///
    /// REVERT-PROBE, RUN: drop the running charge in `collect_attachment_subtree`
    /// and restore the post-walk `cards.len()` check ⇒ this returns `Ok` with
    /// 49 995 000 card entries across 9 999 views (23.2 s, 7.4 GB resident on the
    /// machine this was measured on) instead of `Err`.
    #[test]
    fn the_membership_derivation_refuses_a_worst_case_chain_before_building_it() {
        use crate::game::game_object::GameObject;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(42);
        let owner = PlayerId(0);
        let mut previous: Option<ObjectId> = None;
        for index in 0..=MAX_INTERACTION_LIST_LEN {
            let id = ObjectId(index as u64 + 1);
            let mut object = GameObject::new(
                id,
                CardId(index as u64 + 1),
                owner,
                format!("Chain Link {index}"),
                Zone::Battlefield,
            );
            if let Some(host_id) = previous {
                object.attached_to = Some(AttachTarget::Object(host_id));
                state
                    .objects
                    .get_mut(&host_id)
                    .expect("the host was inserted on the previous iteration")
                    .attachments
                    .push(id);
            }
            state.objects.insert(id, object);
            state.battlefield.push_back(id);
            previous = Some(id);
        }

        // Reach guard: the fixture really is one chain of the intended length,
        // and every view in it would sit inside the per-view cap on its own —
        // without this, an over-long fixture would fail for the wrong reason.
        assert_eq!(state.objects.len(), MAX_INTERACTION_LIST_LEN + 1);
        assert!(
            state
                .objects
                .values()
                .all(|object| object.attachments.len() <= 1),
            "a chain, not a fan: no host may carry two attachments"
        );

        // Compared by discriminant, and reported by SIZE rather than by value:
        // the `Ok` this row exists to reject holds 49 995 000 entries, and a
        // failure that prints them is a failure nobody can read.
        let derived = attachment_views_for_viewer(&state);
        assert!(
            matches!(derived, Err(InteractionReasonCode::PayloadTooLarge)),
            "the derivation must refuse the payload itself rather than hand it to \
             the finalizer to reject — got {} views totalling {} cards",
            derived.as_ref().map_or(0, BTreeMap::len),
            derived.as_ref().map_or(0, |views| views
                .values()
                .map(|view| view.cards.len())
                .sum::<usize>()),
        );
    }
}
