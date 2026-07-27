//! `DrawPayoffPolicy` — makes an on-battlefield "whenever you draw" engine a
//! reason the AI can see to draw EAGERLY.
//!
//! ## The gap this closes
//!
//! CR 121.1: with an engine like The Locust God, Psychosis Crawler, or
//! Niv-Mizzet on the battlefield, every card the AI draws is a repeatable value
//! trigger — an Insect token, a point of damage to each opponent. `card_advantage`
//! values the card itself but not the extra trigger, so the AI will not lean into
//! an extra-draw spell or ability when it has a payoff out. This policy adds that
//! positive signal.
//!
//! ## Performance
//!
//! `verdict()` runs per candidate per search node. The card-local check — does
//! this action actually draw the controller a card (its own `CastFacts`
//! primary/ETB effects, or the activated ability's effects) — runs FIRST and
//! rejects every non-draw action. Only a confirmed draw pays for the battlefield
//! engine scan (a structural trigger match over each permanent's live
//! `trigger_definitions`), and only in a deck whose `activation` floor is already
//! cleared. No affordability sweep, no `find_legal_targets`.

use engine::game::triggers::hypothetical_trigger_fireable;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::features::draw_matters::{
    is_draw_payoff_trigger, is_draw_source_parts, AbilityScope, DrawQuantity, DRAW_MATTERS_FLOOR,
};
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct DrawPayoffPolicy;

/// Cap on how many simultaneous engines are rewarded, so a stacked board can't
/// push a single draw into the critical band.
///
/// `pub(crate)` so the bounded-score regression asserts against this constant
/// rather than a copied literal — raising the cap must move the test with it.
pub(crate) const MAX_REWARDED_ENGINES: usize = 3;

impl TacticalPolicy for DrawPayoffPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::DrawPayoff
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
        if features.draw_matters.commitment < DRAW_MATTERS_FLOOR {
            None
        } else {
            Some(features.draw_matters.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Card-local first: does this action actually draw the controller a card?
        if !candidate_draws_controller(ctx) {
            return PolicyVerdict::neutral(PolicyReason::new("draw_payoff_na"));
        }

        // Only now pay for the battlefield scan. A permanent counts only when it
        // carries a "whenever you draw" trigger (CR 121.1) that is actually LIVE:
        // the engine's `hypothetical_trigger_fireable` authority preflights the
        // trigger's constraint AND its execution target legality (CR 603.3d), so
        // a rate-limited, off-timing, conditional, or no-legal-target engine is
        // not credited value it cannot produce.
        let engines = ctx
            .state
            .battlefield
            .iter()
            .filter(|id| {
                ctx.state.objects.get(id).is_some_and(|obj| {
                    obj.controller == ctx.ai_player
                        && obj.trigger_definitions.iter_unchecked().any(|entry| {
                            is_draw_payoff_trigger(&entry.definition)
                                && hypothetical_trigger_fireable(ctx.state, obj, entry)
                        })
                })
            })
            .count();
        if engines == 0 {
            return PolicyVerdict::neutral(PolicyReason::new("draw_payoff_no_engine"));
        }

        // Each active engine turns this draw into a value trigger — roughly a
        // card-equivalent apiece, capped so one draw stays a preference.
        let rewarded = engines.min(MAX_REWARDED_ENGINES) as f64;
        PolicyVerdict::score(
            ctx.config.policy_penalties.draw_payoff_bonus * rewarded,
            PolicyReason::new("draw_payoff_engine_active").with_fact("engines", engines as i64),
        )
    }
}

/// True when the candidate action draws the controller one or more cards AND
/// that draw can actually be delivered.
///
/// Ordered cheapest-discriminator-first, because `verdict` runs for every
/// `CastSpell` and `ActivateAbility` candidate at every search node. The
/// card-local structural test reads only the candidate's own AST and rejects the
/// overwhelming majority of candidates; only a candidate that structurally draws
/// pays for `can_draw_at_least_one`, which scans battlefield statics and consults
/// the replacement applicability authority. Reversing these two costs every
/// non-draw candidate that scan for nothing.
fn candidate_draws_controller(ctx: &PolicyContext<'_>) -> bool {
    candidate_draws_structurally(ctx) && draw_is_deliverable(ctx)
}

/// CR 121.1 / CR 704.5b + CR 614.6: would a draw right now actually put a card
/// into the AI's hand, emitting the `CardDrawn` event a "whenever you draw"
/// engine rides on? False under a `CantDraw` static or an exhausted
/// `PerTurnDrawLimit`, from an empty library, or when the replacement pipeline
/// removes the draw. Delegates wholly to the engine's `can_draw_at_least_one`
/// authority so the bonus is never added to a no-op draw.
///
/// Deliberately the SECOND gate: it is the expensive one (a battlefield static
/// scan plus replacement applicability), and it is candidate-independent, so it
/// is only worth asking once a candidate is known to draw.
fn draw_is_deliverable(ctx: &PolicyContext<'_>) -> bool {
    engine::game::effects::draw::can_draw_at_least_one(ctx.state, ctx.ai_player)
}

/// CR 121.1 + CR 107.1b: the live-candidate quantity requirement — this draw must
/// resolve to at least one card, or it emits no `CardDrawn` and fires no engine.
/// `source` is the object whose `cost_x_paid` binds an announced `X`, so an
/// un-announced X resolves to zero and the candidate stays neutral.
fn positive_draw_quantity<'a>(
    ctx: &PolicyContext<'a>,
    source: engine::types::identifiers::ObjectId,
) -> DrawQuantity<'a> {
    DrawQuantity::ResolvesPositive {
        state: ctx.state,
        controller: ctx.ai_player,
        source,
    }
}

/// Card-local structural test: does this candidate's own AST draw its controller
/// a card, in a quantity that actually delivers one? Reads the candidate's AST
/// plus the engine's quantity authority; never scans the board.
///
/// * `CastSpell` → the spell's own resolution chain (`CastFacts::primary_effects`)
///   plus its immediate ETB triggers — a cast permanent's *activated* draw
///   ability does not fire on cast, so only these two are inspected.
/// * `ActivateAbility` → the ability at the runtime-enumerated index.
fn candidate_draws_structurally(ctx: &PolicyContext<'_>) -> bool {
    // CR 700.2: a live candidate is scored before its modes are chosen, so only
    // an UNCONDITIONAL draw counts — a modal "choose one — draw / …" must not be
    // credited a draw here.
    match &ctx.candidate.action {
        GameAction::CastSpell { .. } => ctx.cast_facts().is_some_and(|facts| {
            let etb_bodies = facts
                .immediate_etb_triggers
                .iter()
                // CR 603.4: an ETB trigger with an intervening-if condition
                // (Latchkey Faerie's prowl clause) is not preflighted here, so
                // its draw is not credited until it is known it will fire.
                .filter(|trigger| trigger.condition.is_none())
                .filter_map(|trigger| trigger.execute.as_deref());
            is_draw_source_parts(
                facts.primary_effects.iter().copied().chain(etb_bodies),
                AbilityScope::Unconditional,
                &positive_draw_quantity(ctx, facts.object.id),
            )
        }),
        GameAction::ActivateAbility { source_id, .. } => {
            ctx.effective_activated_ability().is_some_and(|ability| {
                is_draw_source_parts(
                    std::iter::once(&ability),
                    AbilityScope::Unconditional,
                    &positive_draw_quantity(ctx, *source_id),
                )
            })
        }
        // CR 601.2 + CR 702.34a: cast-shaped siblings of the plain `CastSpell`
        // seam (alternative costs, madness, miracle, foretell, ninjutsu, copies).
        // `PolicyContext::cast_facts` is populated only for the `CastSpell`
        // announcement seam, so this policy has no AST to classify for these and
        // must report neutral rather than guess. Listed explicitly, not swept
        // into a wildcard: if `cast_facts` later covers one, this arm is where
        // the decision to start crediting it gets made.
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
        // cannot draw its controller a card as part of the candidate itself.
        // Enumerated rather than wildcarded so a newly added `GameAction` fails
        // this match at compile time and forces an intentional classification
        // instead of silently bypassing the draw payoff (CR 121.1).
        GameAction::PassPriority
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
    }
}
