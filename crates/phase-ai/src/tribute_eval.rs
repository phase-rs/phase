//! CR 702.104a-b: Tribute pay/decline evaluator.
//!
//! The chosen opponent of a Tribute creature faces a binary choice as it enters:
//!   - Pay: source enters with N +1/+1 counters.
//!   - Decline: the companion "if tribute wasn't paid" trigger fires.
//!
//! The AI must weigh the board-state value of handing out N +1/+1 counters against
//! the harm of letting the declined-tribute trigger resolve. This is a simple
//! heuristic evaluator — not a full `TacticalPolicy` — invoked from the AI's
//! candidate scoring when it faces a `WaitingFor::TributeChoice`.

use engine::types::ability::{Effect, TargetFilter, TriggerCondition};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::triggers::TriggerMode;

/// Decision returned by `decide`: whether the prompted player should pay tribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TributeDecision {
    Pay,
    Decline,
}

impl TributeDecision {
    pub fn accept(self) -> bool {
        matches!(self, TributeDecision::Pay)
    }
}

/// Decide pay-or-decline for the current `WaitingFor::TributeChoice` on `state`.
/// Returns `None` if `state.waiting_for` is not a tribute choice.
///
/// Heuristic:
///   - Estimate the damage handing out N +1/+1 counters will cause to the
///     prompted player (the opponent) — roughly `2 * count` "board swing"
///     points (each counter is roughly +1 attack/+1 survival).
///   - Estimate the harm of the companion "if tribute wasn't paid" trigger
///     by inspecting the source's trigger definitions for known punishment
///     shapes (DealDamage, DamageEachPlayer, Draw, etc.).
///   - Pay iff counter harm > punishment harm.
///
/// Returns `TributeDecision::Decline` on ties — the trigger may fizzle or be
/// manageable, whereas the counters are permanent on-board presence.
pub fn decide(state: &GameState) -> Option<TributeDecision> {
    let (source_id, count) = match &state.waiting_for {
        WaitingFor::TributeChoice {
            source_id, count, ..
        } => (*source_id, *count),
        _ => return None,
    };

    let counter_harm = estimate_counter_harm(count);
    let decline_harm = estimate_decline_trigger_harm(state, source_id);

    // Pay tribute when the declined-tribute punishment is strictly worse for
    // the prompted opponent than handing out N +1/+1 counters. On ties we lean
    // toward declining — counters are permanent presence, the trigger may
    // fizzle, be countered, or target something unimportant.
    Some(if decline_harm > counter_harm {
        TributeDecision::Pay
    } else {
        TributeDecision::Decline
    })
}

/// Rough per-card value of handing out N +1/+1 counters: each counter boosts
/// power + toughness, so this scales linearly with `count`. Calibration
/// constant `2.0` per counter roughly matches the board-swing weight used by
/// `evaluate_creature` for stat deltas.
fn estimate_counter_harm(count: u32) -> f64 {
    2.0 * count as f64
}

/// Inspect `source_id`'s triggered abilities for ones gated on
/// `TriggerCondition::TributeNotPaid` and score their effect payloads.
///
/// Known punishment shapes and their rough cost-to-prompted-player:
///   - `DealDamage { amount, target: each opponent }` — damage amount
///   - `DamageEachPlayer { amount }` — damage amount (excluding controller)
///   - `Draw { count }` to controller — ~2 points per card
///   - Everything else — treat as a baseline 3 (unknown but present)
fn estimate_decline_trigger_harm(state: &GameState, source_id: ObjectId) -> f64 {
    let Some(source) = state.objects.get(&source_id) else {
        return 0.0;
    };

    let mut harm = 0.0;
    for trigger in source.trigger_definitions.iter_unchecked() {
        let gated_on_tribute = matches!(trigger.condition, Some(TriggerCondition::TributeNotPaid));
        if !gated_on_tribute {
            continue;
        }
        if !matches!(trigger.mode, TriggerMode::ChangesZone) {
            continue;
        }
        let Some(execute) = &trigger.execute else {
            continue;
        };

        // Walk the effect chain scoring each known-shape payload.
        let mut node = Some(execute.as_ref());
        while let Some(def) = node {
            harm += score_effect_harm(state, source_id, &def.effect);
            node = def.sub_ability.as_deref();
        }
    }
    harm
}

fn score_effect_harm(_state: &GameState, _source_id: ObjectId, effect: &Effect) -> f64 {
    use engine::types::ability::QuantityExpr;

    match effect {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value },
            target,
            ..
        } => {
            // Damage to each opponent (filter evaluates per player) is the most
            // common punishment shape.
            let multi_player = target_hits_multiple_players(target);
            let base = *value as f64;
            if multi_player {
                base * 1.5
            } else {
                base
            }
        }
        Effect::DamageEachPlayer {
            amount: QuantityExpr::Fixed { value },
            ..
        } => (*value).max(0) as f64,
        Effect::DamageAll {
            amount: QuantityExpr::Fixed { value },
            ..
        } => (*value).max(0) as f64,
        Effect::Draw { count, .. } => {
            if let QuantityExpr::Fixed { value } = count {
                2.0 * (*value as f64)
            } else {
                2.0
            }
        }
        Effect::DiscardCard { count, .. } => 1.5 * (*count as f64),
        // Static-based punishments (e.g. Fanatic of Xenagos "gets +1/+1 and
        // haste UEOT") — lower than damage because they're temporary.
        Effect::GenericEffect { .. } => 3.0,
        Effect::Pump { .. } => 2.0,
        // Unknown payloads — assume mild.
        _ => 1.5,
    }
}

/// Heuristic: does the filter hit multiple players simultaneously (e.g. an
/// "each opponent" or "each player" filter)? Shape-based inspection — the
/// runtime target resolver would be expensive overkill here.
fn target_hits_multiple_players(filter: &TargetFilter) -> bool {
    let desc = format!("{filter:?}");
    desc.contains("EachOpponent") || desc.contains("EachPlayer")
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::game_object::GameObject;
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::triggers::TriggerMode;
    use engine::types::zones::Zone;
    use engine::types::TriggerDefinition;

    fn seed_source(state: &mut GameState, id: ObjectId) -> &mut GameObject {
        let obj = GameObject::new(
            id,
            CardId(id.0),
            PlayerId(0),
            "Tribute Test".into(),
            Zone::Battlefield,
        );
        state.objects.insert(id, obj);
        state.battlefield.push_back(id);
        state.objects.get_mut(&id).unwrap()
    }

    #[test]
    fn decide_pays_when_punishment_outweighs_counters() {
        let mut state = GameState::new_two_player(1);
        let src_id = ObjectId(100);
        let obj = seed_source(&mut state, src_id);
        // 10-damage punishment per opponent → pay is right.
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::DamageEachPlayer {
                        amount: QuantityExpr::Fixed { value: 10 },
                        player_filter: engine::types::ability::PlayerFilter::Opponent,
                    },
                ))
                .condition(TriggerCondition::TributeNotPaid),
        );
        state.waiting_for = WaitingFor::TributeChoice {
            player: PlayerId(1),
            source_id: src_id,
            count: 1,
        };
        assert_eq!(decide(&state), Some(TributeDecision::Pay));
    }

    #[test]
    fn decide_declines_when_punishment_is_small() {
        let mut state = GameState::new_two_player(1);
        let src_id = ObjectId(101);
        let obj = seed_source(&mut state, src_id);
        // 1-damage "punishment" → declining is better than giving +1/+1 x 3.
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::DamageEachPlayer {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player_filter: engine::types::ability::PlayerFilter::Opponent,
                    },
                ))
                .condition(TriggerCondition::TributeNotPaid),
        );
        state.waiting_for = WaitingFor::TributeChoice {
            player: PlayerId(1),
            source_id: src_id,
            count: 3,
        };
        assert_eq!(decide(&state), Some(TributeDecision::Decline));
    }

    #[test]
    fn decide_declines_when_no_known_trigger() {
        let mut state = GameState::new_two_player(1);
        let src_id = ObjectId(102);
        seed_source(&mut state, src_id);
        state.waiting_for = WaitingFor::TributeChoice {
            player: PlayerId(1),
            source_id: src_id,
            count: 2,
        };
        // No punishment at all — declining avoids the counter-shield altogether.
        assert_eq!(decide(&state), Some(TributeDecision::Decline));
    }

    #[test]
    fn decide_returns_none_when_not_tribute_waiting_for() {
        let state = GameState::new_two_player(1);
        assert!(decide(&state).is_none());
    }
}
