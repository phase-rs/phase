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
//! battlefield engine scan, and only in a deck whose `activation` floor is
//! already cleared. No affordability sweep, no `find_legal_targets`.

use engine::types::ability::AbilityTag;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::features::cycling::CYCLING_PAYOFF_FLOOR;
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

        let Some(feature) = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .map(|f| &f.cycling)
        else {
            return PolicyVerdict::neutral(PolicyReason::new("cycling_payoff_na"));
        };
        if feature.payoff_names.is_empty() {
            return PolicyVerdict::neutral(PolicyReason::new("cycling_payoff_no_engine"));
        }

        // Only now pay for the battlefield scan. Identity lookup (the sanctioned
        // exempt pattern): re-find the structurally-classified engines the AI
        // controls, since `GameObject` carries no `triggers` field.
        let engines = ctx
            .state
            .battlefield
            .iter()
            .filter(|id| {
                ctx.state.objects.get(id).is_some_and(|obj| {
                    obj.controller == ctx.ai_player
                        && feature.payoff_names.iter().any(|name| name == &obj.name)
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
