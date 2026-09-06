//! `DiscardPayoffPolicy` — makes an on-battlefield "whenever you discard" engine
//! a reason the AI can see to pitch cards WILLINGLY.
//!
//! ## The gap this closes
//!
//! CR 701.9: with Archfiend of Ifnir, Bone Miser, Waste Not or Containment
//! Construct on the battlefield, every card the AI discards is a repeatable
//! value trigger. The AI's default instinct is the opposite one — `card_advantage`
//! scores a card leaving hand as a loss, and nothing credits the trigger it
//! fires. So the AI declines its own engine: it routes around rummaging outlets
//! and treats a discard cost as pure downside even when the discard IS the
//! payoff. This policy adds that positive signal.
//!
//! It is deliberately narrow in one direction: it only ever ADDS value for a
//! discard that is about to fire a live engine. It never encourages discarding
//! without one, because without a payoff the AI's instinct is correct.
//!
//! ## Performance
//!
//! `verdict()` runs per candidate per search node. The card-local check — does
//! this action actually discard the controller a card (its own `CastFacts`
//! primary effects, or the activated ability's effects) — runs FIRST and rejects
//! every non-discard action. Only a confirmed discard pays for the battlefield
//! engine scan (a structural trigger match over each permanent's live
//! `trigger_definitions`), and only in a deck whose `activation` floor is already
//! cleared. No affordability sweep, no `find_legal_targets`.

use engine::game::triggers::hypothetical_trigger_fireable;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::features::discard_matters::{
    is_discard_payoff_trigger, is_discard_source_parts, AbilityScope, DiscardQuantity,
    DISCARD_MATTERS_FLOOR,
};
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct DiscardPayoffPolicy;

/// Cap on how many simultaneous engines are rewarded, so a stacked board can't
/// push a single discard into the critical band.
///
/// `pub(crate)` so the bounded-score regression asserts against this constant
/// rather than a copied literal — raising the cap must move the test with it.
pub(crate) const MAX_REWARDED_ENGINES: usize = 3;

impl TacticalPolicy for DiscardPayoffPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::DiscardPayoff
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell, DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if features.discard_matters.commitment < DISCARD_MATTERS_FLOOR {
            None
        } else {
            Some(features.discard_matters.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Card-local first: does this action actually discard the controller a card?
        if !candidate_discards_controller(ctx) {
            return PolicyVerdict::neutral(PolicyReason::new("discard_payoff_na"));
        }

        // Only now pay for the battlefield scan. A permanent counts only when it
        // carries a "whenever you discard" trigger (CR 701.9) that is actually
        // LIVE: the engine's `hypothetical_trigger_fireable` authority preflights
        // the trigger's constraint AND its execution target legality (CR 603.3d),
        // so a rate-limited, off-timing, conditional, or no-legal-target engine is
        // not credited value it cannot produce.
        let engines = ctx
            .state
            .battlefield
            .iter()
            .filter(|id| {
                ctx.state.objects.get(id).is_some_and(|obj| {
                    obj.controller == ctx.ai_player
                        && obj.trigger_definitions.iter_unchecked().any(|entry| {
                            is_discard_payoff_trigger(&entry.definition)
                                && hypothetical_trigger_fireable(ctx.state, obj, entry)
                        })
                })
            })
            .count();
        if engines == 0 {
            return PolicyVerdict::neutral(PolicyReason::new("discard_payoff_no_engine"));
        }

        // Each active engine turns this discard into a value trigger — roughly a
        // card-equivalent apiece, capped so one discard stays a preference.
        let rewarded = engines.min(MAX_REWARDED_ENGINES) as f64;
        PolicyVerdict::score(
            ctx.config.policy_penalties.discard_payoff_bonus * rewarded,
            PolicyReason::new("discard_payoff_engine_active").with_fact("engines", engines as i64),
        )
    }
}

/// CR 701.9: the live-candidate quantity requirement — this discard must resolve
/// to at least one card, or it moves nothing and fires no engine. `source` is the
/// object whose `cost_x_paid` binds an announced `X`, so an un-announced X
/// resolves to zero and the candidate stays neutral.
fn positive_discard_quantity<'a>(
    ctx: &PolicyContext<'a>,
    source: engine::types::identifiers::ObjectId,
) -> DiscardQuantity<'a> {
    DiscardQuantity::ResolvesPositive {
        state: ctx.state,
        controller: ctx.ai_player,
        source,
    }
}

/// Card-local structural test: does this candidate's own AST discard its
/// controller a card, in a quantity that actually moves one?
///
/// * `CastSpell` → the spell's own resolution chain (`CastFacts::primary_effects`).
/// * `ActivateAbility` → the ability at the runtime-enumerated index, which is
///   where the rummaging outlets live (Anje, Wild Mongrel, Faithless-Looting
///   style activated pitch).
///
/// CR 700.2: a live candidate is scored before its modes are chosen, so only an
/// UNCONDITIONAL discard counts — a modal "choose one — discard / …" must not be
/// credited a discard here.
fn candidate_discards_controller(ctx: &PolicyContext<'_>) -> bool {
    match &ctx.candidate.action {
        GameAction::CastSpell { .. } => ctx.cast_facts().is_some_and(|facts| {
            is_discard_source_parts(
                facts.primary_effects.iter().copied(),
                AbilityScope::Unconditional,
                &positive_discard_quantity(ctx, facts.object.id),
            )
        }),
        GameAction::ActivateAbility { source_id, .. } => {
            ctx.effective_activated_ability().is_some_and(|ability| {
                is_discard_source_parts(
                    std::iter::once(&ability),
                    AbilityScope::Unconditional,
                    &positive_discard_quantity(ctx, *source_id),
                )
            })
        }
        // CR 601.2 + CR 702.34a: cast-shaped siblings of the plain `CastSpell`
        // seam, all declined a discard credit — for two different reasons.
        //
        // `Foretell` / `PlayFaceDown` / `ActivateNinjutsu` / `CastPreparedCopy` /
        // `CastParadigmCopy` are outside the cast family, so
        // `PolicyContext::cast_facts` is empty for them and this policy has no
        // AST to classify — neutral rather than guess.
        //
        // The five alternative/free-cast variants DO carry `cast_facts` since
        // the cast-family widening, so the AST is now available for them and
        // this arm is where the decision to read it gets made. It stays `false`
        // for now: widening this policy is its own change with its own test.
        // `CastSpellAsMadness` stays `false` on the merits regardless — a
        // madness cast is the payoff the DISCARD already earned, not a new
        // discard, and crediting it would double-count one event.
        GameAction::Foretell { .. }
        | GameAction::PlayFaceDown { .. }
        | GameAction::ActivateNinjutsu { .. }
        | GameAction::CastSpellAsSneak { .. }
        | GameAction::CastSpellAsWebSlinging { .. }
        | GameAction::CastSpellForFree { .. }
        | GameAction::CastSpellAsMiracle { .. }
        | GameAction::CastSpellAsMadness { .. }
        | GameAction::CastPreparedCopy { .. }
        | GameAction::CastParadigmCopy { .. } => false,
        // Every remaining action: not a spell cast or ability activation, so it
        // cannot discard its controller a card as part of the candidate itself.
        // Enumerated rather than wildcarded so a newly added `GameAction` fails
        // this match at compile time and forces an intentional classification
        // instead of silently bypassing the discard payoff (CR 701.9).
        GameAction::PassPriority
        | GameAction::BeginResolveAll { .. }
        | GameAction::RespondResolveAllConsent { .. }
        | GameAction::RevokeResolveAllConsent { .. }
        | GameAction::ChooseMeldPair { .. }
        | GameAction::ChooseEntryAttackTarget { .. }
        | GameAction::PlayLand { .. }
        | GameAction::DeclareAttackers { .. }
        | GameAction::DeclareBlockers { .. }
        | GameAction::ChooseUntap { .. }
        | GameAction::ChooseExert { .. }
        | GameAction::ChooseEnlist { .. }
        | GameAction::ChooseClashOpponent { .. }
        | GameAction::ChooseZoneOpponentChooser { .. }
        | GameAction::ChoosePileOpponent { .. }
        | GameAction::ChooseAnnouncingOpponent { .. }
        | GameAction::ChooseGiftRecipient { .. }
        | GameAction::ChooseAssistPlayer { .. }
        | GameAction::CommitAssistPayment { .. }
        | GameAction::MulliganDecision { .. }
        | GameAction::ReorderHand { .. }
        | GameAction::TapLandForMana { .. }
        | GameAction::ActivateManaSource { .. }
        | GameAction::BackToManaPayment
        | GameAction::UntapLandForMana { .. }
        | GameAction::SpendPoolMana { .. }
        | GameAction::UnspendPoolMana { .. }
        | GameAction::SelectCards { .. }
        | GameAction::ChooseRemoveCounterCostDistribution { .. }
        | GameAction::SelectCoinFlips { .. }
        | GameAction::ChooseOutsideGameCards { .. }
        | GameAction::SelectTargets { .. }
        | GameAction::ChooseTarget { .. }
        | GameAction::ChooseReplacement { .. }
        | GameAction::ChooseEntryController { .. }
        | GameAction::OrderTriggers { .. }
        | GameAction::CancelCast
        | GameAction::Equip { .. }
        | GameAction::CrewVehicle { .. }
        | GameAction::ActivateStation { .. }
        | GameAction::SaddleMount { .. }
        | GameAction::Transform { .. }
        | GameAction::TurnFaceUp { .. }
        | GameAction::SubmitSideboard { .. }
        | GameAction::ChoosePlayDraw { .. }
        | GameAction::ChooseOption { .. }
        | GameAction::SubmitVoteCandidate { .. }
        | GameAction::SubmitSpellbookDraft { .. }
        | GameAction::SubmitPilePartition { .. }
        | GameAction::ChoosePile { .. }
        | GameAction::ChooseBranch { .. }
        | GameAction::SubmitLifeRedistribution { .. }
        | GameAction::ChooseDamageSource { .. }
        | GameAction::SelectModes { .. }
        | GameAction::DecideOptionalCost { .. }
        | GameAction::ChooseAdventureFace { .. }
        | GameAction::ChooseModalFace { .. }
        | GameAction::ChooseAlternativeCast { .. }
        | GameAction::ChooseCastingVariant { .. }
        | GameAction::KeepAllCopyTargets
        | GameAction::ChoosePermanentTypeSlot { .. }
        | GameAction::DecideOptionalEffect { .. }
        | GameAction::RespondToSpliceOffer { .. }
        | GameAction::DecideOptionalEffectAndRemember { .. }
        | GameAction::PayUnlessCost { .. }
        | GameAction::ChooseUnlessCostBranch { .. }
        | GameAction::ChooseActivationCostBranch { .. }
        | GameAction::PayCombatTax { .. }
        | GameAction::ChooseRingBearer { .. }
        | GameAction::ChoosePair { .. }
        | GameAction::ChooseDungeon { .. }
        | GameAction::ChooseDungeonRoom { .. }
        | GameAction::UnlockRoomDoor { .. }
        | GameAction::RollPlanarDie
        | GameAction::ChooseRoomDoor { .. }
        | GameAction::TapForConvoke { .. }
        | GameAction::HarmonizeTap { .. }
        | GameAction::DeclareCompanion { .. }
        | GameAction::CompanionToHand
        | GameAction::DiscoverChoice { .. }
        | GameAction::GraveyardPaidCastChoice { .. }
        | GameAction::CascadeChoice { .. }
        | GameAction::RippleChoice { .. }
        | GameAction::FreeCastWindowChoice { .. }
        | GameAction::ChooseTopOrBottom { .. }
        | GameAction::ChooseMutateMergeSide { .. }
        | GameAction::CipherEncode { .. }
        | GameAction::ChooseLegend { .. }
        | GameAction::ChooseBattleProtector { .. }
        | GameAction::SetAutoPass { .. }
        | GameAction::CancelAutoPass
        | GameAction::SetPhaseStops { .. }
        | GameAction::SetPriorityPassingMode { .. }
        | GameAction::SetPriorityYield { .. }
        | GameAction::SetMayTriggerAutoChoice { .. }
        | GameAction::SetTriggerOrderTemplate { .. }
        | GameAction::AssignCombatDamage { .. }
        | GameAction::AssignBlockerDamage { .. }
        | GameAction::DistributeAmong { .. }
        | GameAction::ChooseCounterMoveDistribution { .. }
        | GameAction::ChooseCountersToRemove { .. }
        | GameAction::SubmitPayAmount { .. }
        | GameAction::RetargetSpell { .. }
        | GameAction::LearnDecision { .. }
        | GameAction::SelectCategoryPermanents { .. }
        | GameAction::ChooseKeptCreatures { .. }
        | GameAction::ChooseKeptPermanents { .. }
        | GameAction::ChooseX { .. }
        | GameAction::SubmitPhyrexianChoices { .. }
        | GameAction::ChooseManaColor { .. }
        | GameAction::PayManaAbilityMana { .. }
        | GameAction::ChooseSpecializeColor { .. }
        | GameAction::PassParadigmOffer
        | GameAction::Debug(..)
        | GameAction::GrantDebugPermission { .. }
        | GameAction::RevokeDebugPermission { .. }
        | GameAction::Concede { .. }
        | GameAction::DeclareShortcut { .. }
        | GameAction::RespondToShortcut { .. }
        | GameAction::DeclineShortcut
        | GameAction::PrecastCopyShortcut { .. }
        | GameAction::EndContinuousEffect { .. } => false,
        GameAction::ChooseResolutionOptionalPaymentBranch { .. } => false,
    }
}
