//! `CyclingPayoffPolicy` — makes an on-battlefield "whenever you cycle" engine a
//! reason the AI can see to cycle EAGERLY.
//!
//! ## The gap this closes
//!
//! CR 702.29a: cycling is card-neutral selection, so the generic activated-
//! ability prior undervalues it and [`CyclingDisciplinePolicy`](super::cycling_discipline)
//! only adds *patience* (don't cycle away a needed land); `self_cost_value`
//! explicitly defers cycling value (`self_cost_cycling_deferred`). Neither sees
//! the upside: with an engine like Astral Drift or Drannith Stinger on the
//! battlefield (CR 702.29c/d), every cycle is a repeatable value trigger — exile
//! a creature, ping each opponent, draw. This policy adds that positive signal,
//! which composes with the discipline penalty so a payoff deck cycles into its
//! engine while a smoothing-only deck stays patient.
//!
//! ## Performance
//!
//! `verdict()` runs per candidate per search node. The card-local check — the
//! candidate is a `Cycling`-tagged activation — runs FIRST and rejects every
//! other activation. Only a confirmed cycling activation pays for the
//! battlefield engine scan (a structural trigger match over each permanent's
//! live `trigger_definitions`), and only in a deck whose `activation` floor is
//! already cleared. Target legality is deliberately NOT checked — that would
//! mean a per-candidate `find_legal_targets` sweep, and it would wrongly drop
//! no-target payoffs like Drannith Stinger ("deals damage to each opponent").

use engine::types::ability::AbilityTag;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use engine::game::game_object::GameObject;
use engine::types::ability::{TriggerConstraint, TriggerEntry};
use engine::types::phase::Phase;

use crate::features::cycling::{is_cycle_payoff_trigger, CYCLING_PAYOFF_FLOOR};
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct CyclingPayoffPolicy;

/// Cap on how many simultaneous engines are rewarded, so a stacked board can't
/// push a single cycle into the critical band.
const MAX_REWARDED_ENGINES: usize = 3;

impl TacticalPolicy for CyclingPayoffPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::CyclingPayoff
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if features.cycling.commitment < CYCLING_PAYOFF_FLOOR {
            None
        } else {
            Some(features.cycling.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Card-local first: only a Cycling activation is in scope (CR 702.29a).
        let Some(ability) = ctx.effective_activated_ability() else {
            return PolicyVerdict::neutral(PolicyReason::new("cycling_payoff_na"));
        };
        if ability.ability_tag != Some(AbilityTag::Cycling) {
            return PolicyVerdict::neutral(PolicyReason::new("cycling_payoff_na"));
        }

        // Only now pay for the battlefield scan. Re-classify each permanent the
        // AI controls STRUCTURALLY against its live `trigger_definitions` (CR
        // 702.29c/d) — a name match is not enough, the object must actually
        // carry the "whenever you cycle" trigger to produce value.
        let engines = ctx
            .state
            .battlefield
            .iter()
            .filter(|id| {
                ctx.state.objects.get(id).is_some_and(|obj| {
                    obj.controller == ctx.ai_player
                        && obj.trigger_definitions.iter_unchecked().any(|entry| {
                            is_cycle_payoff_trigger(&entry.definition)
                                && trigger_still_fireable(ctx.state, obj, entry)
                        })
                })
            })
            .count();
        if engines == 0 {
            return PolicyVerdict::neutral(PolicyReason::new("cycling_payoff_no_engine"));
        }

        // Each active engine turns this cycle into a value trigger — roughly a
        // card-equivalent apiece, capped so one cycle stays a preference, not a
        // game-deciding swing.
        let rewarded = engines.min(MAX_REWARDED_ENGINES) as f64;
        PolicyVerdict::score(
            ctx.config.policy_penalties.cycling_payoff_bonus * rewarded,
            PolicyReason::new("cycling_payoff_engine_active").with_fact("engines", engines as i64),
        )
    }
}

/// Whether `obj`'s cycling-payoff trigger `entry` could still fire this turn.
/// Exhaustive over `TriggerConstraint` (no wildcard): the once/timing limits are
/// evaluated against authoritative state (fired-trigger ledgers, `active_player`,
/// phase); event/count-dependent constraints the policy can't confirm at decision
/// time are treated as NOT fireable so the payoff is never over-credited.
fn trigger_still_fireable(
    state: &engine::types::game_state::GameState,
    obj: &GameObject,
    entry: &TriggerEntry,
) -> bool {
    let Some(constraint) = &entry.definition.constraint else {
        return true; // no constraint — always fireable
    };
    match constraint {
        // CR 603.4 / CR 603.2: already-consumed "once" limits.
        TriggerConstraint::OncePerTurn => !state
            .triggers_fired_this_turn
            .contains(&obj.trigger_definition_ref(entry)),
        TriggerConstraint::OncePerGame => !state
            .triggers_fired_this_game
            .contains(&obj.trigger_definition_ref(entry)),
        // Turn/phase timing — evaluable from turn state alone.
        TriggerConstraint::OnlyDuringYourTurn => state.active_player == obj.controller,
        TriggerConstraint::OnlyDuringOpponentsTurn => state.active_player != obj.controller,
        TriggerConstraint::OnlyDuringYourMainPhase => {
            state.active_player == obj.controller
                && matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
        }
        // Event- or count-dependent: not confirmable at decision time, so never
        // over-credit the payoff.
        TriggerConstraint::MaxTimesPerTurn { .. }
        | TriggerConstraint::NthSpellThisTurn { .. }
        | TriggerConstraint::NthDrawThisTurn { .. }
        | TriggerConstraint::OncePerOpponentPerTurn
        | TriggerConstraint::AtClassLevel { .. }
        | TriggerConstraint::EventSourceControlledBy { .. } => false,
    }
}
