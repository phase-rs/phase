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

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::features::draw_matters::{
    is_draw_payoff_parts, is_draw_source_parts, DRAW_MATTERS_FLOOR,
};
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct DrawPayoffPolicy;

/// Cap on how many simultaneous engines are rewarded, so a stacked board can't
/// push a single draw into the critical band.
const MAX_REWARDED_ENGINES: usize = 3;

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

        // Only now pay for the battlefield scan. Re-classify each permanent the
        // AI controls STRUCTURALLY against its live `trigger_definitions` (CR
        // 121.1) — the object must actually carry a "whenever you draw" trigger
        // to produce value.
        let engines = ctx
            .state
            .battlefield
            .iter()
            .filter(|id| {
                ctx.state.objects.get(id).is_some_and(|obj| {
                    obj.controller == ctx.ai_player
                        && is_draw_payoff_parts(
                            obj.trigger_definitions
                                .iter_unchecked()
                                .map(|entry| &entry.definition),
                        )
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

/// True when the candidate action draws the controller one or more cards.
///
/// * `CastSpell` → the spell's own resolution chain (`CastFacts::primary_effects`)
///   plus its immediate ETB triggers — a cast permanent's *activated* draw
///   ability does not fire on cast, so only these two are inspected.
/// * `ActivateAbility` → the ability at the runtime-enumerated index.
fn candidate_draws_controller(ctx: &PolicyContext<'_>) -> bool {
    match &ctx.candidate.action {
        GameAction::CastSpell { .. } => ctx.cast_facts().is_some_and(|facts| {
            let etb_bodies = facts
                .immediate_etb_triggers
                .iter()
                .filter_map(|trigger| trigger.execute.as_deref());
            is_draw_source_parts(facts.primary_effects.iter().copied().chain(etb_bodies))
        }),
        GameAction::ActivateAbility { .. } => ctx
            .effective_activated_ability()
            .is_some_and(|ability| is_draw_source_parts(std::iter::once(&ability))),
        _ => false,
    }
}
