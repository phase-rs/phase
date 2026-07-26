//! `DevotionPolicy` — makes CR 700.5 pip density a resource the AI can see.
//!
//! ## The defect this closes
//!
//! CR 700.5: devotion to a color is the number of that color's mana symbols
//! among the mana costs of permanents you control. It is the payoff currency
//! for the Theros gods (not creatures below their threshold), Gray Merchant
//! drains, and X = devotion scalers. The AI's evaluation models mana value and
//! board presence but not pip density, so between two comparable permanents it
//! could not prefer the double-pip one, and it could not see that casting one
//! more colored permanent flips a dormant god into a lethal beater.
//!
//! ## Why the god-threshold crossing is a distinct branch
//!
//! Below a god's threshold the god is not a creature; the cast that reaches the
//! threshold turns a dead enchantment into a large indestructible body — a
//! multi-card swing, not the marginal +1 that the previous pip was. That
//! discontinuity is scored as its own term (`devotion_god_activation`), the same
//! "last missing piece" structure `graveyard_types` uses for the fourth card
//! type. Every other pip is a smooth preference (`devotion_pip_progress`).
//!
//! ## Performance
//!
//! `verdict()` runs per candidate per search node. The card-local check — pips
//! in the candidate's own mana cost, a handful of shard matches — runs FIRST
//! and rejects every non-permanent and off-color cast. Only a confirmed
//! primary-color permanent pays for `count_devotion`, one pass over the AI's
//! battlefield permanents (the CR 700.5 runtime authority). No affordability
//! sweep, no `find_legal_targets`.

use engine::game::devotion::count_devotion;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::mana::ManaColor;
use engine::types::player::PlayerId;

use crate::features::devotion::{cost_devotion_pips, DEVOTION_FLOOR};
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct DevotionPolicy;

impl TacticalPolicy for DevotionPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::Devotion
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if features.devotion.commitment < DEVOTION_FLOOR {
            None
        } else {
            Some(features.devotion.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let feature = match ctx.context.session.features.get(&ctx.ai_player) {
            Some(f) => &f.devotion,
            None => return PolicyVerdict::neutral(PolicyReason::new("devotion_na")),
        };
        // No payoff colors ⇒ nothing to score against.
        if feature.primary_colors.is_empty() && feature.gates.is_empty() {
            return PolicyVerdict::neutral(PolicyReason::new("devotion_na"));
        }

        // Card-local first: the cast must be a permanent (CR 110.4 — only a
        // permanent contributes devotion).
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("devotion_na"));
        };
        let Some(obj) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new("devotion_na"));
        };
        if !obj
            .card_types
            .core_types
            .iter()
            .any(|t| t.is_permanent_type())
        {
            return PolicyVerdict::neutral(PolicyReason::new("devotion_na"));
        }

        // Cheapest board-free discriminator: does the cast add any pip toward a
        // color set the deck actually cares about (primary demand or any gate)?
        // Skip the battlefield scans for every off-color permanent.
        let mut relevant: Vec<ManaColor> = feature.primary_colors.clone();
        for gate in &feature.gates {
            for c in &gate.colors {
                if !relevant.contains(c) {
                    relevant.push(*c);
                }
            }
        }
        if cost_devotion_pips(&obj.mana_cost, &relevant) == 0 {
            return PolicyVerdict::neutral(PolicyReason::new("devotion_off_color"));
        }

        let pip_scalar = ctx.config.policy_penalties.devotion_pip_progress;

        // CR 700.5: evaluate each god's gate against its OWN color set —
        // combined devotion for a two-color god (Athreos W+B, Xenagos R+G),
        // hybrids counted once. A cast that reaches a gate's combined threshold
        // turns that god on; several can flip at once, and a cast crossing a
        // lower gate matters even when a higher one it does not reach exists.
        let crossed = feature
            .gates
            .iter()
            .filter(|gate| {
                let current = count_devotion(ctx.state, ctx.ai_player, &gate.colors);
                let added = cost_devotion_pips(&obj.mana_cost, &gate.colors);
                current < gate.threshold && current + added >= gate.threshold
            })
            .count() as u32;

        // The smooth-pip component is measured against the deck's primary demand.
        let primary_added = cost_devotion_pips(&obj.mana_cost, &feature.primary_colors);
        let primary_devotion = count_devotion(ctx.state, ctx.ai_player, &feature.primary_colors);

        if crossed > 0 {
            let activation = ctx.config.policy_penalties.devotion_god_activation;
            return PolicyVerdict::score(
                activation * f64::from(crossed) + pip_scalar * f64::from(primary_added),
                PolicyReason::new("devotion_god_activation")
                    .with_fact("devotion", primary_devotion as i64)
                    .with_fact("gods_activated", crossed as i64),
            );
        }

        if primary_added == 0 {
            // Pips only toward an already-met or off-primary gate: no smooth
            // progress to score once no gate crosses.
            return PolicyVerdict::neutral(PolicyReason::new("devotion_off_color"));
        }

        PolicyVerdict::score(
            pip_scalar * f64::from(primary_added),
            PolicyReason::new("devotion_pip_progress")
                .with_fact("devotion", primary_devotion as i64)
                .with_fact("added", primary_added as i64),
        )
    }
}
