//! Self-bounce target selection — when a "return a land you control" ETB forces
//! the AI to return one of its own lands, pick the least-costly one and NEVER
//! the land that just entered (which loops).
//!
//! ## The defect this closes (#4730)
//!
//! Azorius Chancery and the rest of the Ravnica/MOM bounce-land ("Karoo") cycle
//! enter and their ETB returns "a land you control" to hand (CR 608.2c, chosen
//! at resolution). No policy scored that choice, so the AI kept returning the
//! bounce-land it had just played — putting it back in hand to be replayed and
//! bounced again, the tempo loop the reporter observed. `land_sequencing` fixed
//! the *sequencing* half and its own doc-comment deferred this half verbatim:
//! "the AI should return the least-useful land, never the just-played
//! bounce-land — that needs its own target-selection policy."
//!
//! ## Heuristic (building block, not a card fix)
//!
//! The choice surfaces as a `WaitingFor::EffectZoneChoice` returning battlefield
//! lands you control to hand; each candidate is a `SelectCards` selection.
//! Detection is structural (no card names) — it covers every "return a land you
//! control" bouncer. Each land in the selection is scored by how little
//! returning it costs:
//! * the ability's own source — the just-played bounce-land — takes a strong
//!   penalty, so it is chosen only when it is the sole eligible land (which
//!   breaks the loop);
//! * an already-tapped land is preferred (its mana is spent this turn, so
//!   replaying it next turn costs the least tempo);
//! * an untapped land is mildly penalized (returning it forfeits mana still
//!   available this turn) but still beats looping the bounce-land.

use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

/// Penalty for returning the ability's own source — the just-played bounce-land.
/// Returning it re-buys the land drop for nothing and loops, so it must lose to
/// any other land the pool offers.
pub(crate) const RETURN_SOURCE_PENALTY: f64 = -3.0;
/// Bonus for returning an already-tapped land: its mana is spent this turn, so
/// replaying it next turn costs the least tempo.
pub(crate) const RETURN_TAPPED_BONUS: f64 = 1.0;
/// Penalty for returning an untapped land: doing so forfeits mana still
/// available this turn. Milder than the source penalty — an untapped non-source
/// land is still a better return than looping the bounce-land.
pub(crate) const RETURN_UNTAPPED_PENALTY: f64 = -0.5;

pub struct SelfBounceTargetPolicy;

impl SelfBounceTargetPolicy {
    /// Desirability of returning `land_id` to hand; higher = better to bounce.
    pub(crate) fn return_desirability(
        state: &GameState,
        land_id: ObjectId,
        source_id: ObjectId,
    ) -> f64 {
        // CR 608.2c: never bounce the permanent that generated the choice — that
        // is the tempo loop this policy exists to break.
        if land_id == source_id {
            return RETURN_SOURCE_PENALTY;
        }
        match state.objects.get(&land_id) {
            Some(land) if land.tapped => RETURN_TAPPED_BONUS,
            Some(_) => RETURN_UNTAPPED_PENALTY,
            None => 0.0,
        }
    }
}

impl TacticalPolicy for SelfBounceTargetPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SelfBounceTarget
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        // `WaitingFor::EffectZoneChoice` routes through the ActivateAbility
        // catch-all bucket (see `decision_kind::classify`).
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Universal; the verdict's bounce-choice guard self-gates.
        // activation-constant: self-bounce target choice, universal.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let na = || PolicyVerdict::neutral(PolicyReason::new("self_bounce_target_na"));

        let GameAction::SelectCards { cards } = &ctx.candidate.action else {
            return na();
        };
        // CR 608.2c: a resolution-time "return a land you control to hand"
        // choice — from the battlefield, to hand, scoped to the AI.
        let WaitingFor::EffectZoneChoice {
            player,
            source_id,
            effect_kind: EffectKind::ChangeZone,
            zone: Zone::Battlefield,
            destination: Some(Zone::Hand),
            cards: pool,
            ..
        } = &ctx.decision.waiting_for
        else {
            return na();
        };
        if *player != ctx.ai_player {
            return na();
        }
        // Restrict to the "return a LAND you control" class: every eligible
        // object is a land the AI controls. A value self-bounce of a creature
        // (blink, save-from-removal) is intentionally left untouched.
        let all_own_lands = pool.iter().all(|id| {
            ctx.state.objects.get(id).is_some_and(|o| {
                o.controller == ctx.ai_player && o.card_types.core_types.contains(&CoreType::Land)
            })
        });
        if pool.is_empty() || !all_own_lands || cards.is_empty() {
            return na();
        }

        let delta: f64 = cards
            .iter()
            .map(|&id| Self::return_desirability(ctx.state, id, *source_id))
            .sum();

        PolicyVerdict::score(
            delta,
            PolicyReason::new("self_bounce_target")
                .with_fact("returns_source", i64::from(cards.contains(source_id))),
        )
    }
}
