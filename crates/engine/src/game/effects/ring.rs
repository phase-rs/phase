use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 701.54: The Ring tempts you.
///
/// CR 701.54a: When the Ring tempts you, choose a creature you control.
/// That creature becomes your Ring-bearer.
///
/// CR 701.54c: Each time the Ring tempts you, your ring level increases by one
/// (to a maximum of four levels, 0-indexed as 0–3).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let controller = ability.controller;

    // CR 701.54b: Increment ring level, capping at 4 (the ring has 4 tiers).
    // Level 0 = never tempted (no abilities). Levels 1–4 unlock progressive tiers.
    let level = state.ring_level.entry(controller).or_insert(0);
    if *level < 4 {
        *level += 1;
    }

    // Emit the event so triggers can fire.
    events.push(GameEvent::RingTemptsYou {
        player_id: controller,
    });

    // CR 701.54a: Collect candidate creatures controlled by this player.
    let candidates: Vec<_> = state
        .battlefield
        .iter()
        .filter_map(|&oid| {
            let obj = state.objects.get(&oid)?;
            if obj.controller == controller
                && obj.zone == Zone::Battlefield
                && obj.card_types.core_types.contains(&CoreType::Creature)
            {
                Some(oid)
            } else {
                None
            }
        })
        .collect();

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RingTemptsYou,
        source_id: ability.source_id,
    });

    if candidates.is_empty() {
        // No creatures — ring tempts but no ring-bearer selection.
        return Ok(());
    }

    if candidates.len() == 1 {
        // Only one creature — auto-select as ring-bearer.
        state.ring_bearer.insert(controller, Some(candidates[0]));
        crate::game::layers::mark_layers_full(state);
        return Ok(());
    }

    // Multiple candidates — ask the player to choose.
    state.waiting_for = WaitingFor::ChooseRingBearer {
        player: controller,
        candidates,
    };

    Ok(())
}

/// CR 701.54e: A player's Ring-bearer designation is valid only while that
/// creature remains on the battlefield under that player's control.
pub(crate) fn is_current_ring_bearer(
    state: &GameState,
    player: crate::types::player::PlayerId,
    object_id: ObjectId,
) -> bool {
    if state.ring_bearer.get(&player).copied().flatten() != Some(object_id) {
        return false;
    }
    state.objects.get(&object_id).is_some_and(|obj| {
        obj.zone == Zone::Battlefield
            && obj.controller == player
            && obj.card_types.core_types.contains(&CoreType::Creature)
    })
}

pub(crate) fn ring_bearer_for(
    state: &GameState,
    player: crate::types::player::PlayerId,
) -> Option<ObjectId> {
    let object_id = state.ring_bearer.get(&player).copied().flatten()?;
    is_current_ring_bearer(state, player, object_id).then_some(object_id)
}

pub(crate) fn clear_ring_bearer_if_object(state: &mut GameState, object_id: ObjectId) {
    let mut changed = false;
    state.ring_bearer.retain(|_, bearer| {
        if matches!(bearer, Some(id) if *id == object_id) {
            changed = true;
            false
        } else {
            true
        }
    });
    if changed {
        crate::game::layers::mark_layers_full(state);
    }
}

pub(crate) fn normalize_ring_bearers(state: &mut GameState) -> bool {
    let stale: Vec<_> = state
        .ring_bearer
        .iter()
        .filter_map(|(&player, bearer)| {
            let object_id = bearer.as_ref().copied()?;
            (!is_current_ring_bearer(state, player, object_id)).then_some(player)
        })
        .collect();

    if stale.is_empty() {
        return false;
    }

    for player in stale {
        state.ring_bearer.remove(&player);
    }
    crate::game::layers::mark_layers_full(state);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::effects::resolve_ability_chain;
    use crate::game::zones::create_object;
    use crate::game::{combat, layers, triggers, zones};
    use crate::types::ability::{Effect, PlayerFilter, ResolvedAbility, TargetFilter};
    use crate::types::card_type::{CardType, CoreType, Supertype};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;

    fn make_creature(state: &mut GameState, card_id: u64, controller: PlayerId) -> ObjectId {
        let oid = create_object(
            state,
            CardId(card_id),
            controller,
            format!("Creature {card_id}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };
        oid
    }

    #[test]
    fn ring_tempts_emits_effect_resolved_event() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(Effect::RingTemptsYou, vec![], ObjectId(1), PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::RingTemptsYou,
                ..
            }
        )));
    }

    #[test]
    fn ring_level_caps_at_four() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(Effect::RingTemptsYou, vec![], ObjectId(1), PlayerId(0));

        // Tempt 5 times — level should cap at 4
        for _ in 0..5 {
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();
        }

        assert_eq!(state.ring_level[&PlayerId(0)], 4);
    }

    #[test]
    fn ring_tempts_auto_selects_single_creature() {
        let mut state = GameState::new_two_player(42);
        let creature_id = make_creature(&mut state, 1, PlayerId(0));
        let ability =
            ResolvedAbility::new(Effect::RingTemptsYou, vec![], ObjectId(99), PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.ring_bearer.get(&PlayerId(0)),
            Some(&Some(creature_id))
        );
    }

    #[test]
    fn ring_tempts_prompts_choice_for_multiple_creatures() {
        let mut state = GameState::new_two_player(42);
        make_creature(&mut state, 1, PlayerId(0));
        make_creature(&mut state, 2, PlayerId(0));
        let ability =
            ResolvedAbility::new(Effect::RingTemptsYou, vec![], ObjectId(99), PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ChooseRingBearer { .. }
        ));
    }

    #[test]
    fn ring_tempts_no_creatures_still_increments_level() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(Effect::RingTemptsYou, vec![], ObjectId(1), PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.ring_level[&PlayerId(0)], 1);
        assert_eq!(state.ring_bearer.get(&PlayerId(0)), None);
    }

    #[test]
    fn ring_bearer_is_legendary_from_the_ring_emblem() {
        let mut state = GameState::new_two_player(42);
        let creature_id = make_creature(&mut state, 1, PlayerId(0));
        state.ring_level.insert(PlayerId(0), 1);
        state.ring_bearer.insert(PlayerId(0), Some(creature_id));
        state.layers_dirty.mark_full();

        layers::evaluate_layers(&mut state);

        assert!(state.objects[&creature_id]
            .card_types
            .supertypes
            .contains(&Supertype::Legendary));
    }

    #[test]
    fn ring_bearer_cant_be_blocked_by_greater_power_creature() {
        let mut state = GameState::new_two_player(42);
        let attacker = make_creature(&mut state, 1, PlayerId(0));
        let blocker = make_creature(&mut state, 2, PlayerId(1));
        state.objects.get_mut(&attacker).unwrap().power = Some(2);
        state.objects.get_mut(&blocker).unwrap().power = Some(3);
        state.ring_level.insert(PlayerId(0), 1);
        state.ring_bearer.insert(PlayerId(0), Some(attacker));

        assert!(!combat::can_block_pair(&state, blocker, attacker));

        state.objects.get_mut(&blocker).unwrap().power = Some(2);
        assert!(combat::can_block_pair(&state, blocker, attacker));
    }

    #[test]
    fn ring_bearer_designation_clears_when_object_leaves_battlefield() {
        let mut state = GameState::new_two_player(42);
        let creature_id = make_creature(&mut state, 1, PlayerId(0));
        state.ring_level.insert(PlayerId(0), 1);
        state.ring_bearer.insert(PlayerId(0), Some(creature_id));
        let mut events = Vec::new();

        zones::move_to_zone(&mut state, creature_id, Zone::Graveyard, &mut events);

        assert_eq!(state.ring_bearer.get(&PlayerId(0)), None);
    }

    #[test]
    fn ring_level_two_attack_queues_draw_then_discard_trigger() {
        let mut state = GameState::new_two_player(42);
        let bearer = make_creature(&mut state, 1, PlayerId(0));
        state.ring_level.insert(PlayerId(0), 2);
        state.ring_bearer.insert(PlayerId(0), Some(bearer));

        triggers::process_triggers(
            &mut state,
            &[GameEvent::AttackersDeclared {
                attacker_ids: vec![bearer],
                defending_player: PlayerId(1),
                attacks: Vec::new(),
            }],
        );

        let ability = state.stack.last().unwrap().ability().unwrap();
        assert!(matches!(ability.effect, Effect::Draw { .. }));
        assert!(matches!(
            ability.sub_ability.as_ref().map(|sub| &sub.effect),
            Some(Effect::Discard { .. })
        ));
    }

    #[test]
    fn ring_level_three_blocked_queues_delayed_sacrifice_trigger() {
        let mut state = GameState::new_two_player(42);
        let bearer = make_creature(&mut state, 1, PlayerId(0));
        let blocker = make_creature(&mut state, 2, PlayerId(1));
        state.ring_level.insert(PlayerId(0), 3);
        state.ring_bearer.insert(PlayerId(0), Some(bearer));

        triggers::process_triggers(
            &mut state,
            &[GameEvent::BlockersDeclared {
                assignments: vec![(blocker, bearer)],
            }],
        );

        let ability = state.stack.last().unwrap().ability().unwrap().clone();
        assert_eq!(ability.controller, PlayerId(0));
        let Effect::CreateDelayedTrigger { effect, .. } = &ability.effect else {
            panic!("expected The Ring level 3 to create a delayed trigger");
        };
        let Effect::Sacrifice { target, .. } = effect.effect.as_ref() else {
            panic!("expected delayed trigger to sacrifice the blocker");
        };
        let TargetFilter::And { filters } = target else {
            panic!("expected delayed trigger to scope sacrifice by blocker controller");
        };
        assert!(filters
            .iter()
            .any(|filter| filter == &TargetFilter::SpecificObject { id: blocker }));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(state.delayed_triggers[0].controller, PlayerId(0));
        assert_eq!(state.delayed_triggers[0].ability.controller, PlayerId(0));
        assert_eq!(
            state.delayed_triggers[0].ability.scoped_player,
            Some(PlayerId(1))
        );

        triggers::check_delayed_triggers(
            &mut state,
            &[GameEvent::PhaseChanged {
                phase: Phase::EndCombat,
            }],
        );

        let delayed_ability = state.stack.last().unwrap().ability().unwrap().clone();
        assert_eq!(delayed_ability.controller, PlayerId(0));
        assert_eq!(delayed_ability.scoped_player, Some(PlayerId(1)));
        resolve_ability_chain(&mut state, &delayed_ability, &mut events, 0).unwrap();
        assert_eq!(state.objects[&blocker].zone, Zone::Graveyard);
    }

    #[test]
    fn ring_level_four_combat_damage_queues_each_opponent_loses_life_trigger() {
        let mut state = GameState::new_two_player(42);
        let bearer = make_creature(&mut state, 1, PlayerId(0));
        state.ring_level.insert(PlayerId(0), 4);
        state.ring_bearer.insert(PlayerId(0), Some(bearer));

        triggers::process_triggers(
            &mut state,
            &[GameEvent::CombatDamageDealtToPlayer {
                player_id: PlayerId(1),
                source_amounts: vec![(bearer, 3)],
                total_damage: 3,
            }],
        );

        let ability = state.stack.last().unwrap().ability().unwrap();
        assert!(matches!(
            ability.effect,
            Effect::LoseLife {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
        assert_eq!(ability.player_scope, Some(PlayerFilter::Opponent));
    }

    const RING_TRIGGER_ORACLE: &str = "Whenever the Ring tempts you, draw a card.";

    #[test]
    fn ring_tempts_you_observer_trigger_is_collected_and_draws() {
        use crate::game::trigger_index;
        use crate::parser::oracle_trigger::parse_trigger_line;

        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let watcher = make_creature(&mut state, 10, PlayerId(0));
        {
            let obj = state.objects.get_mut(&watcher).unwrap();
            obj.trigger_definitions
                .push(parse_trigger_line(RING_TRIGGER_ORACLE, "Ring Watcher"));
        }
        trigger_index::reindex_object_triggers(&mut state, watcher);

        let mut events = Vec::new();
        let ability =
            ResolvedAbility::new(Effect::RingTemptsYou, vec![], ObjectId(99), PlayerId(0));
        resolve(&mut state, &ability, &mut events).unwrap();

        triggers::process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            1,
            "Whenever the Ring tempts you must place a triggered ability on the stack"
        );
        let draw = state.stack.last().unwrap().ability().unwrap();
        assert!(matches!(
            draw.effect,
            Effect::Draw {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
    }
}
