use engine::game::game_object::GameObject;
use engine::game::players;
use engine::types::ability::Effect;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::cast_facts::cast_facts_for_action;
use crate::combat_ai;
use crate::config::PolicyPenalties;
use crate::eval::{evaluate_creature, threat_level};

use super::context::PolicyContext;

pub(crate) fn is_own_main_phase(ctx: &PolicyContext<'_>) -> bool {
    engine::game::turn_control::turn_decision_maker(ctx.state) == ctx.ai_player
        && ctx.state.stack.is_empty()
        && matches!(
            ctx.state.phase,
            Phase::PreCombatMain | Phase::PostCombatMain
        )
}

pub(crate) fn board_presence_score(object: &GameObject) -> f64 {
    let mut score = 0.0;

    if object.card_types.core_types.contains(&CoreType::Creature) {
        let power = object.power.unwrap_or(0).max(0) as f64;
        let toughness = object.toughness.unwrap_or(0).max(0) as f64;
        score += ((power + toughness) / 8.0).min(0.45);
        score += keyword_pressure(object) * 0.04;
    } else if object
        .card_types
        .core_types
        .contains(&CoreType::Planeswalker)
    {
        score += 0.28 + object.loyalty.unwrap_or(0) as f64 / 20.0;
    } else if object.card_types.core_types.iter().any(|core_type| {
        matches!(
            core_type,
            CoreType::Artifact | CoreType::Battle | CoreType::Enchantment
        )
    }) {
        score += 0.16;
    }

    score.min(0.65)
}

pub(crate) fn best_proactive_cast_score(ctx: &PolicyContext<'_>) -> f64 {
    ctx.decision
        .candidates
        .iter()
        .filter_map(|candidate| cast_facts_for_action(ctx.state, &candidate.action, ctx.ai_player))
        .map(|facts| {
            let mut score = board_presence_score(facts.object);
            if !facts.immediate_etb_triggers.is_empty() || !facts.immediate_replacements.is_empty()
            {
                score += 0.16;
            }
            if facts.has_search_library {
                score += 0.24;
            }
            if facts.has_draw {
                score += 0.1;
            }
            if facts.has_direct_removal_text {
                score += 0.14;
            }
            score
        })
        .fold(0.0, f64::max)
}

pub(crate) fn visible_opponent_creature_value(state: &GameState, ai_player: PlayerId) -> f64 {
    let opponents = players::opponents(state, ai_player);
    state
        .battlefield
        .iter()
        .filter_map(|object_id| {
            let object = state.objects.get(object_id)?;
            if opponents.contains(&object.controller)
                && object.card_types.core_types.contains(&CoreType::Creature)
            {
                Some(
                    evaluate_creature(state, *object_id)
                        * (threat_level(state, ai_player, object.controller) + 0.5),
                )
            } else {
                None
            }
        })
        .fold(0.0, f64::max)
}

pub(crate) fn battlefield_pressure_delta(state: &GameState, ai_player: PlayerId) -> f64 {
    let mut ours = 0.0;
    let mut theirs = 0.0;

    for object_id in &state.battlefield {
        let Some(object) = state.objects.get(object_id) else {
            continue;
        };
        if !object.card_types.core_types.contains(&CoreType::Creature) {
            continue;
        }
        let value = evaluate_creature(state, *object_id);
        if object.controller == ai_player {
            ours += value;
        } else {
            theirs += value;
        }
    }

    ours - theirs
}

/// Sum of opponent untapped creature power, weighted by evasion.
/// Creatures AI cannot block count at full power; blockable ones at 50%.
pub(crate) fn opponent_lethal_damage(state: &GameState, ai_player: PlayerId) -> i32 {
    let opponents = players::opponents(state, ai_player);

    // Collect AI's untapped creatures for blocking checks
    let ai_blockers: Vec<&GameObject> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.controller == ai_player
                && !obj.tapped
                && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .collect();

    let mut total = 0i32;
    for &obj_id in &state.battlefield {
        let Some(obj) = state.objects.get(&obj_id) else {
            continue;
        };
        if !opponents.contains(&obj.controller)
            || obj.tapped
            || !obj.card_types.core_types.contains(&CoreType::Creature)
        {
            continue;
        }
        let power = obj.power.unwrap_or(0);
        let can_be_blocked = ai_blockers
            .iter()
            .any(|blocker| combat_ai::can_block_check(blocker, obj));
        if can_be_blocked {
            // Blockable creatures contribute half power (some will get through)
            total += power / 2;
        } else {
            total += power;
        }
    }
    total
}

/// Whether any of ai_player's untapped creatures can legally block the given creature.
/// Delegates to `combat_ai::can_block_check` for flying/reach/shadow rules.
pub(crate) fn ai_can_block(state: &GameState, ai_player: PlayerId, attacker_id: ObjectId) -> bool {
    let Some(attacker) = state.objects.get(&attacker_id) else {
        return false;
    };
    state.battlefield.iter().any(|&id| {
        state.objects.get(&id).is_some_and(|obj| {
            obj.controller == ai_player
                && !obj.tapped
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && combat_ai::can_block_check(obj, attacker)
        })
    })
}

/// Value of a permanent for sacrifice-ordering decisions.
/// Higher values mean the permanent is more costly to sacrifice.
pub(crate) fn sacrifice_cost(
    state: &GameState,
    obj_id: ObjectId,
    penalties: &PolicyPenalties,
) -> f64 {
    let Some(obj) = state.objects.get(&obj_id) else {
        return 0.0;
    };
    if obj.card_types.core_types.contains(&CoreType::Land) {
        return penalties.sacrifice_land_penalty;
    }
    // Token creatures: use creature eval if they have meaningful stats,
    // otherwise use flat token cost (Treasures, Maps, Clues, etc.)
    if obj.is_token {
        if obj.card_types.core_types.contains(&CoreType::Creature) {
            return evaluate_creature(state, obj_id).max(penalties.sacrifice_token_cost);
        }
        return penalties.sacrifice_token_cost;
    }
    if obj.card_types.core_types.contains(&CoreType::Creature) {
        return evaluate_creature(state, obj_id);
    }
    // Other permanents: scale by mana value, capped
    (obj.mana_cost.mana_value() as f64).min(4.0)
}

/// Count spells in hand with a Counter effect ability.
pub(crate) fn count_counterspells_in_hand(state: &GameState, player: PlayerId) -> usize {
    state.players[player.0 as usize]
        .hand
        .iter()
        .filter(|&&obj_id| {
            state.objects.get(&obj_id).is_some_and(|obj| {
                obj.abilities
                    .iter()
                    .any(|ability| matches!(&*ability.effect, Effect::Counter { .. }))
            })
        })
        .count()
}

fn keyword_pressure(object: &GameObject) -> f64 {
    object
        .keywords
        .iter()
        .map(|keyword| match keyword {
            Keyword::Flying
            | Keyword::Trample
            | Keyword::Vigilance
            | Keyword::Menace
            | Keyword::Lifelink
            | Keyword::Deathtouch
            | Keyword::FirstStrike
            | Keyword::DoubleStrike
            | Keyword::Haste => 1.0,
            _ => 0.0,
        })
        .sum::<f64>()
        .min(3.0)
}
