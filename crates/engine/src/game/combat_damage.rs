use std::collections::HashSet;

use crate::game::combat::{CombatState, DamageAssignment, DamageTarget};
use crate::game::effects::life::apply_life_gain;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::sba;
use crate::game::triggers;
use crate::types::ability::TargetRef;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::proposed_event::ProposedEvent;

/// Resolve combat damage with first strike / double strike support (CR 510.1).
/// CR 702.7b: If any creature has first strike or double strike, two damage sub-steps run.
/// Between sub-steps: SBAs are checked and triggers processed.
pub fn resolve_combat_damage(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let combat = match &state.combat {
        Some(c) => c.clone(),
        None => return,
    };

    let has_first_or_double = combat.attackers.iter().any(|a| {
        state
            .objects
            .get(&a.object_id)
            .map(|o| o.has_keyword(&Keyword::FirstStrike) || o.has_keyword(&Keyword::DoubleStrike))
            .unwrap_or(false)
    }) || combat.blocker_to_attacker.keys().any(|blocker_id| {
        state
            .objects
            .get(blocker_id)
            .map(|o| o.has_keyword(&Keyword::FirstStrike) || o.has_keyword(&Keyword::DoubleStrike))
            .unwrap_or(false)
    });

    if has_first_or_double {
        // First strike damage step
        let first_strike_events = first_strike_damage_step(state);
        events.extend(first_strike_events.iter().cloned());

        // Mark first strike done
        if let Some(c) = &mut state.combat {
            c.first_strike_done = true;
        }

        // SBAs between damage steps
        sba::check_state_based_actions(state, events);
        triggers::process_triggers(state, &first_strike_events);

        // Regular damage step
        let regular_events = regular_damage_step(state);
        events.extend(regular_events.iter().cloned());
        sba::check_state_based_actions(state, events);
        triggers::process_triggers(state, &regular_events);
    } else {
        // Single damage step
        let regular_events = regular_damage_step(state);
        events.extend(regular_events.iter().cloned());
        sba::check_state_based_actions(state, events);
        triggers::process_triggers(state, &regular_events);
    }
}

/// CR 702.7b: First strike damage step — only FirstStrike and DoubleStrike creatures deal damage.
fn first_strike_damage_step(state: &mut GameState) -> Vec<GameEvent> {
    let combat = match &state.combat {
        Some(c) => c.clone(),
        None => return Vec::new(),
    };

    let mut all_assignments: Vec<(ObjectId, DamageAssignment)> = Vec::new();

    // Attackers with first/double strike
    for attacker_info in &combat.attackers {
        let obj = match state.objects.get(&attacker_info.object_id) {
            Some(o)
                if o.zone == crate::types::zones::Zone::Battlefield
                    && (o.has_keyword(&Keyword::FirstStrike)
                        || o.has_keyword(&Keyword::DoubleStrike)) =>
            {
                o
            }
            _ => continue,
        };
        let power = obj.power.unwrap_or(0).max(0) as u32;
        if power == 0 {
            continue;
        }
        let has_deathtouch = obj.has_keyword(&Keyword::Deathtouch);
        let has_trample = obj.has_keyword(&Keyword::Trample);
        let assignments = assign_attacker_damage(
            state,
            attacker_info.object_id,
            &combat,
            power,
            has_deathtouch,
            has_trample,
        );
        for a in assignments {
            all_assignments.push((attacker_info.object_id, a));
        }
    }

    // Blockers with first/double strike
    for (blocker_id, attacker_id) in &combat.blocker_to_attacker {
        let obj = match state.objects.get(blocker_id) {
            Some(o)
                if o.zone == crate::types::zones::Zone::Battlefield
                    && (o.has_keyword(&Keyword::FirstStrike)
                        || o.has_keyword(&Keyword::DoubleStrike)) =>
            {
                o
            }
            _ => continue,
        };
        let power = obj.power.unwrap_or(0).max(0) as u32;
        if power == 0 {
            continue;
        }
        all_assignments.push((
            *blocker_id,
            DamageAssignment {
                target: DamageTarget::Object(*attacker_id),
                amount: power,
            },
        ));
    }

    apply_combat_damage(state, &all_assignments)
}

/// CR 510.2: Regular damage step — creatures without FirstStrike (already dealt) + DoubleStrike creatures deal damage.
fn regular_damage_step(state: &mut GameState) -> Vec<GameEvent> {
    let combat = match &state.combat {
        Some(c) => c.clone(),
        None => return Vec::new(),
    };
    let first_strike_was_done = combat.first_strike_done;

    let mut all_assignments: Vec<(ObjectId, DamageAssignment)> = Vec::new();

    // Attackers: those without FirstStrike (they haven't dealt yet), plus DoubleStrike (deal again)
    for attacker_info in &combat.attackers {
        let obj = match state.objects.get(&attacker_info.object_id) {
            Some(o) if o.zone == crate::types::zones::Zone::Battlefield => o,
            _ => continue,
        };

        // Skip if this creature has FirstStrike only (already dealt) and first strike step ran
        if first_strike_was_done
            && obj.has_keyword(&Keyword::FirstStrike)
            && !obj.has_keyword(&Keyword::DoubleStrike)
        {
            continue;
        }

        // Skip if no first strike step happened and creature doesn't need to deal
        // (all creatures deal in regular step if no first strike step)

        let power = obj.power.unwrap_or(0).max(0) as u32;
        if power == 0 {
            continue;
        }
        let has_deathtouch = obj.has_keyword(&Keyword::Deathtouch);
        let has_trample = obj.has_keyword(&Keyword::Trample);
        let assignments = assign_attacker_damage(
            state,
            attacker_info.object_id,
            &combat,
            power,
            has_deathtouch,
            has_trample,
        );
        for a in assignments {
            all_assignments.push((attacker_info.object_id, a));
        }
    }

    // Blockers: same logic
    for (blocker_id, attacker_id) in &combat.blocker_to_attacker {
        let obj = match state.objects.get(blocker_id) {
            Some(o) if o.zone == crate::types::zones::Zone::Battlefield => o,
            _ => continue,
        };

        if first_strike_was_done
            && obj.has_keyword(&Keyword::FirstStrike)
            && !obj.has_keyword(&Keyword::DoubleStrike)
        {
            continue;
        }

        let power = obj.power.unwrap_or(0).max(0) as u32;
        if power == 0 {
            continue;
        }
        all_assignments.push((
            *blocker_id,
            DamageAssignment {
                target: DamageTarget::Object(*attacker_id),
                amount: power,
            },
        ));
    }

    apply_combat_damage(state, &all_assignments)
}

/// Determine how an attacker assigns its damage (CR 510.1c).
fn assign_attacker_damage(
    state: &GameState,
    attacker_id: ObjectId,
    combat: &CombatState,
    power: u32,
    has_deathtouch: bool,
    has_trample: bool,
) -> Vec<DamageAssignment> {
    let defending_player = combat
        .attackers
        .iter()
        .find(|a| a.object_id == attacker_id)
        .map(|a| a.defending_player)
        .unwrap_or(crate::types::player::PlayerId(1));

    let blockers = combat.blocker_assignments.get(&attacker_id);

    let blockers = blockers.filter(|b| !b.is_empty());

    match blockers {
        None => {
            // Unblocked: all damage to defending player
            vec![DamageAssignment {
                target: DamageTarget::Player(defending_player),
                amount: power,
            }]
        }
        Some(blockers) => {
            if blockers.len() == 1 {
                if has_trample {
                    // CR 702.19b: Trample — assign lethal to blocker, excess to defending player.
                    let lethal = lethal_damage_needed(state, blockers[0], has_deathtouch);
                    let to_blocker = power.min(lethal);
                    let excess = power.saturating_sub(to_blocker);
                    let mut result = vec![DamageAssignment {
                        target: DamageTarget::Object(blockers[0]),
                        amount: to_blocker,
                    }];
                    if excess > 0 {
                        result.push(DamageAssignment {
                            target: DamageTarget::Player(defending_player),
                            amount: excess,
                        });
                    }
                    result
                } else {
                    // Single blocker without trample: all damage to blocker
                    vec![DamageAssignment {
                        target: DamageTarget::Object(blockers[0]),
                        amount: power,
                    }]
                }
            } else {
                // Multiple blockers (ordered)
                let mut remaining = power;
                let mut result = Vec::new();

                for (i, &blocker_id) in blockers.iter().enumerate() {
                    if remaining == 0 {
                        break;
                    }
                    let lethal = lethal_damage_needed(state, blocker_id, has_deathtouch);
                    let is_last = i == blockers.len() - 1;

                    if has_trample && is_last {
                        // Trample: assign lethal to last blocker, excess to player
                        let to_blocker = remaining.min(lethal);
                        result.push(DamageAssignment {
                            target: DamageTarget::Object(blocker_id),
                            amount: to_blocker,
                        });
                        let excess = remaining.saturating_sub(to_blocker);
                        if excess > 0 {
                            result.push(DamageAssignment {
                                target: DamageTarget::Player(defending_player),
                                amount: excess,
                            });
                        }
                        remaining = 0;
                    } else if !has_trample && is_last {
                        // Without trample: dump all remaining to last blocker
                        result.push(DamageAssignment {
                            target: DamageTarget::Object(blocker_id),
                            amount: remaining,
                        });
                        remaining = 0;
                    } else {
                        // Assign lethal to this blocker, move on
                        let to_blocker = remaining.min(lethal);
                        result.push(DamageAssignment {
                            target: DamageTarget::Object(blocker_id),
                            amount: to_blocker,
                        });
                        remaining = remaining.saturating_sub(to_blocker);
                    }
                }

                result
            }
        }
    }
}

/// How much damage is needed to kill this creature.
/// CR 702.2c: Deathtouch — any amount of damage from a deathtouch source is lethal.
fn lethal_damage_needed(
    state: &GameState,
    object_id: ObjectId,
    source_has_deathtouch: bool,
) -> u32 {
    if source_has_deathtouch {
        // CR 702.2c + CR 702.19b: With deathtouch, 1 damage is lethal.
        return 1;
    }
    state
        .objects
        .get(&object_id)
        .map(|obj| {
            let toughness = obj.toughness.unwrap_or(0).max(0) as u32;
            toughness.saturating_sub(obj.damage_marked)
        })
        .unwrap_or(1)
}

/// Apply combat damage assignments: mark damage on creatures, reduce player life, handle lifelink.
/// All damage goes through replace_event for replacement effect interception.
fn apply_combat_damage(
    state: &mut GameState,
    assignments: &[(ObjectId, DamageAssignment)],
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut combat_damage_to_players: Vec<(crate::types::player::PlayerId, Vec<ObjectId>)> =
        Vec::new();

    for (source_id, assignment) in assignments {
        let (
            source_has_deathtouch,
            source_has_lifelink,
            source_has_wither,
            source_has_infect,
            source_controller,
            source_is_commander,
        ) = state
            .objects
            .get(source_id)
            .map(|o| {
                (
                    o.has_keyword(&Keyword::Deathtouch),
                    o.has_keyword(&Keyword::Lifelink),
                    o.has_keyword(&Keyword::Wither),
                    o.has_keyword(&Keyword::Infect),
                    o.controller,
                    o.is_commander,
                )
            })
            .unwrap_or((
                false,
                false,
                false,
                false,
                crate::types::player::PlayerId(0),
                false,
            ));

        let target_ref = match &assignment.target {
            DamageTarget::Object(id) => TargetRef::Object(*id),
            DamageTarget::Player(id) => TargetRef::Player(*id),
        };

        // CR 702.16b: Protection prevents damage from sources with matching quality.
        if let DamageTarget::Object(target_id) = &assignment.target {
            if let (Some(target_obj), Some(source_obj)) =
                (state.objects.get(target_id), state.objects.get(source_id))
            {
                if crate::game::keywords::protection_prevents_from(target_obj, source_obj) {
                    continue;
                }
            }
        }

        let proposed = ProposedEvent::Damage {
            source_id: *source_id,
            target: target_ref,
            amount: assignment.amount,
            is_combat: true,
            applied: HashSet::new(),
        };

        let actual_amount = match replacement::replace_event(state, proposed, &mut events) {
            ReplacementResult::Execute(event) => {
                if let ProposedEvent::Damage {
                    target: ref t,
                    amount,
                    ..
                } = event
                {
                    match t {
                        TargetRef::Object(target_id) => {
                            if source_has_wither || source_has_infect {
                                // CR 702.79b + CR 702.89b: Wither/Infect apply -1/-1 counters instead of damage.
                                if let Some(target_obj) = state.objects.get_mut(target_id) {
                                    let counter =
                                        crate::game::game_object::CounterType::Minus1Minus1;
                                    let entry = target_obj.counters.entry(counter).or_insert(0);
                                    *entry += amount;
                                    if source_has_deathtouch {
                                        target_obj.dealt_deathtouch_damage = true;
                                    }
                                }
                                state.layers_dirty = true;
                            } else if let Some(target_obj) = state.objects.get_mut(target_id) {
                                if target_obj
                                    .card_types
                                    .core_types
                                    .contains(&crate::types::card_type::CoreType::Planeswalker)
                                {
                                    // Damage to planeswalker removes loyalty
                                    let current = target_obj.loyalty.unwrap_or(0);
                                    target_obj.loyalty = Some(current.saturating_sub(amount));
                                } else {
                                    target_obj.damage_marked += amount;
                                    if source_has_deathtouch {
                                        target_obj.dealt_deathtouch_damage = true;
                                    }
                                }
                            }
                            events.push(GameEvent::DamageDealt {
                                source_id: *source_id,
                                target: TargetRef::Object(*target_id),
                                amount,
                                is_combat: true,
                            });
                        }
                        TargetRef::Player(player_id) => {
                            if source_has_infect {
                                // CR 702.89b: Infect — deals damage to players as poison counters.
                                if let Some(player) =
                                    state.players.iter_mut().find(|p| p.id == *player_id)
                                {
                                    player.poison_counters += amount;
                                }
                            } else {
                                crate::game::effects::life::apply_damage_life_loss(
                                    state,
                                    *player_id,
                                    amount,
                                    &mut events,
                                );
                            }
                            events.push(GameEvent::DamageDealt {
                                source_id: *source_id,
                                target: TargetRef::Player(*player_id),
                                amount,
                                is_combat: true,
                            });

                            let player_sources = combat_damage_to_players
                                .iter_mut()
                                .find(|(damaged_player, _)| *damaged_player == *player_id)
                                .map(|(_, source_ids)| source_ids);
                            if let Some(source_ids) = player_sources {
                                if !source_ids.contains(source_id) {
                                    source_ids.push(*source_id);
                                }
                            } else {
                                combat_damage_to_players.push((*player_id, vec![*source_id]));
                            }

                            // Commander damage tracking
                            if source_is_commander && amount > 0 {
                                if let Some(entry) = state
                                    .commander_damage
                                    .iter_mut()
                                    .find(|e| e.player == *player_id && e.commander == *source_id)
                                {
                                    entry.damage += amount;
                                } else {
                                    state.commander_damage.push(
                                        crate::types::game_state::CommanderDamageEntry {
                                            player: *player_id,
                                            commander: *source_id,
                                            damage: amount,
                                        },
                                    );
                                }
                            }
                        }
                    }
                    amount
                } else {
                    0
                }
            }
            ReplacementResult::Prevented => 0,
            ReplacementResult::NeedsChoice(_) => {
                // Combat damage NeedsChoice is an edge case; for now, skip
                0
            }
        };

        // CR 702.15b: Lifelink — source's controller gains life equal to damage dealt.
        // Route through the replacement pipeline so effects like Leyline of Hope apply.
        if source_has_lifelink && actual_amount > 0 {
            apply_life_gain(state, source_controller, actual_amount, &mut events);
        }
    }

    for (player_id, source_ids) in combat_damage_to_players {
        events.push(GameEvent::CombatDamageDealtToPlayer {
            player_id,
            source_ids,
        });
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, ControllerRef, Effect, QuantityExpr, TriggerDefinition, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state
    }

    fn create_creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.entered_battlefield_turn = Some(1);
        id
    }

    fn setup_combat(
        state: &mut GameState,
        attackers: Vec<ObjectId>,
        blocker_assignments: Vec<(ObjectId, Vec<ObjectId>)>,
    ) {
        let mut combat = CombatState {
            attackers: attackers
                .iter()
                .map(|&id| AttackerInfo {
                    object_id: id,
                    defending_player: PlayerId(1),
                })
                .collect(),
            ..Default::default()
        };
        for (attacker_id, blockers) in blocker_assignments {
            for &blocker_id in &blockers {
                combat.blocker_to_attacker.insert(blocker_id, attacker_id);
            }
            combat.blocker_assignments.insert(attacker_id, blockers);
        }
        state.combat = Some(combat);
    }

    #[test]
    fn unblocked_attacker_deals_damage_to_player() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.players[1].life, 18); // 20 - 2
    }

    #[test]
    fn blocked_attacker_deals_damage_to_blocker() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Attacker dealt 2 to blocker
        assert_eq!(state.objects[&blocker].damage_marked, 2);
        // Blocker dealt 0 to attacker
        assert_eq!(state.objects[&attacker].damage_marked, 0);
        // No player damage
        assert_eq!(state.players[1].life, 20);
    }

    #[test]
    fn mutual_combat_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.objects[&attacker].damage_marked, 2);
        assert_eq!(state.objects[&blocker].damage_marked, 2);
    }

    #[test]
    fn first_strike_kills_before_regular_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Knight", 3, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::FirstStrike);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // First strike dealt 3 damage (lethal) to blocker
        // SBAs ran between steps -- blocker should have been destroyed
        // Blocker can't deal damage back in regular step (dead)
        // Attacker should have 0 damage
        assert_eq!(state.objects[&attacker].damage_marked, 0);
        // Blocker should be in graveyard (SBAs ran between steps)
        assert!(!state.battlefield.contains(&blocker));
    }

    #[test]
    fn double_strike_deals_damage_twice() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Knight", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::DoubleStrike);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // 3 + 3 = 6 damage to player
        assert_eq!(state.players[1].life, 14);
    }

    #[test]
    fn trample_assigns_lethal_then_excess_to_player() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fatty", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // 2 to blocker (lethal), 3 to player (trample excess)
        assert_eq!(state.objects[&blocker].damage_marked, 2);
        assert_eq!(state.players[1].life, 17);
    }

    #[test]
    fn trample_deathtouch_assigns_one_to_each_blocker() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "DT Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Deathtouch);
        let blocker1 = create_creature(&mut state, PlayerId(1), "Bear1", 2, 2);
        let blocker2 = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker1, blocker2])],
        );

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // With deathtouch, 1 to each blocker is lethal; 3 excess tramples to player
        assert_eq!(state.objects[&blocker1].damage_marked, 1);
        assert_eq!(state.objects[&blocker2].damage_marked, 1);
        assert_eq!(state.players[1].life, 17);
    }

    #[test]
    fn lifelink_gains_life_on_combat_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lifelinker", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // 3 damage to defending player
        assert_eq!(state.players[1].life, 17);
        // 3 life gained by controller
        assert_eq!(state.players[0].life, 23);
    }

    #[test]
    fn combat_no_combat_state_is_noop() {
        let mut state = setup();
        state.combat = None;
        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert!(events.is_empty());
    }

    #[test]
    fn multiple_blockers_without_trample_all_damage_to_blockers() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fatty", 5, 5);
        let blocker1 = create_creature(&mut state, PlayerId(1), "Bear1", 2, 2);
        let blocker2 = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker1, blocker2])],
        );

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Without trample: 2 lethal to first blocker, remaining 3 to second blocker
        assert_eq!(state.objects[&blocker1].damage_marked, 2);
        assert_eq!(state.objects[&blocker2].damage_marked, 3);
        // No damage to player
        assert_eq!(state.players[1].life, 20);
    }

    #[test]
    fn deathtouch_marks_flag_on_target() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "DT", 1, 1);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Deathtouch);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert!(state.objects[&blocker].dealt_deathtouch_damage);
    }

    #[test]
    fn wither_applies_minus_counters_to_creature_instead_of_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Wither", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Wither);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Wither: 3 -1/-1 counters instead of damage_marked
        assert_eq!(state.objects[&blocker].damage_marked, 0);
        assert_eq!(
            state.objects[&blocker]
                .counters
                .get(&crate::game::game_object::CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            3
        );
    }

    #[test]
    fn wither_to_player_deals_normal_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Wither", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Wither);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Wither does NOT give poison to players, just normal damage
        assert_eq!(state.players[1].life, 17);
        assert_eq!(state.players[1].poison_counters, 0);
    }

    #[test]
    fn infect_applies_minus_counters_to_creature() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Infector", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Infect);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Infect: -1/-1 counters on creature
        assert_eq!(state.objects[&blocker].damage_marked, 0);
        assert_eq!(
            state.objects[&blocker]
                .counters
                .get(&crate::game::game_object::CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            3
        );
    }

    #[test]
    fn infect_to_player_gives_poison_no_life_loss() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Infector", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Infect);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Infect: poison counters, no life loss
        assert_eq!(state.players[1].life, 20);
        assert_eq!(state.players[1].poison_counters, 3);
    }

    #[test]
    fn lifelink_works_with_infect() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "InfectLinker", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Infect);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Infect gives poison, but lifelink still triggers
        assert_eq!(state.players[1].poison_counters, 3);
        assert_eq!(state.players[0].life, 23); // gained 3 life
    }

    #[test]
    fn commander_damage_tracked_when_commander_hits_player() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Commander", 5, 5);
        state.objects.get_mut(&attacker).unwrap().is_commander = true;
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Commander dealt 5 damage to player 1
        assert_eq!(state.players[1].life, 15);
        // Commander damage tracked
        assert_eq!(state.commander_damage.len(), 1);
        assert_eq!(state.commander_damage[0].player, PlayerId(1));
        assert_eq!(state.commander_damage[0].commander, attacker);
        assert_eq!(state.commander_damage[0].damage, 5);
    }

    #[test]
    fn commander_damage_accumulates_over_multiple_combats() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Commander", 3, 3);
        state.objects.get_mut(&attacker).unwrap().is_commander = true;
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert_eq!(state.commander_damage[0].damage, 3);

        // Second combat
        state.combat = None;
        state.objects.get_mut(&attacker).unwrap().tapped = false;
        setup_combat(&mut state, vec![attacker], vec![]);
        events.clear();
        resolve_combat_damage(&mut state, &mut events);

        // Accumulated: 3 + 3 = 6
        assert_eq!(state.commander_damage[0].damage, 6);
    }

    #[test]
    fn non_commander_damage_not_tracked() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        // is_commander defaults to false
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.players[1].life, 18);
        assert!(state.commander_damage.is_empty());
    }

    #[test]
    fn different_commanders_tracked_separately() {
        let mut state = setup();
        let cmd_a = create_creature(&mut state, PlayerId(0), "Cmd A", 3, 3);
        state.objects.get_mut(&cmd_a).unwrap().is_commander = true;
        let cmd_b = create_creature(&mut state, PlayerId(0), "Cmd B", 2, 2);
        state.objects.get_mut(&cmd_b).unwrap().is_commander = true;
        setup_combat(&mut state, vec![cmd_a, cmd_b], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Two separate entries
        assert_eq!(state.commander_damage.len(), 2);
        let entry_a = state
            .commander_damage
            .iter()
            .find(|e| e.commander == cmd_a)
            .unwrap();
        let entry_b = state
            .commander_damage
            .iter()
            .find(|e| e.commander == cmd_b)
            .unwrap();
        assert_eq!(entry_a.damage, 3);
        assert_eq!(entry_b.damage, 2);
    }

    #[test]
    fn one_or_more_combat_damage_trigger_fires_once_per_damage_step() {
        let mut state = setup();
        let watcher = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Professional Face-Breaker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&watcher)
            .unwrap()
            .trigger_definitions
            .push({
                let mut trigger = TriggerDefinition::new(TriggerMode::DamageDoneOnceByController)
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Spell,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ));
                trigger.valid_source = Some(crate::types::ability::TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ));
                trigger.valid_target = Some(crate::types::ability::TargetFilter::Player);
                trigger
            });

        let attacker_a = create_creature(&mut state, PlayerId(0), "Attacker A", 2, 2);
        let attacker_b = create_creature(&mut state, PlayerId(0), "Attacker B", 3, 3);
        setup_combat(&mut state, vec![attacker_a, attacker_b], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.stack.len(), 1);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                GameEvent::CombatDamageDealtToPlayer {
                    player_id,
                    source_ids,
                } if *player_id == PlayerId(1)
                    && source_ids.len() == 2
                    && source_ids.contains(&attacker_a)
                    && source_ids.contains(&attacker_b)
            )
        }));
    }

    #[test]
    fn one_or_more_combat_damage_trigger_fires_in_each_double_strike_step() {
        let mut state = setup();
        let watcher = create_object(
            &mut state,
            CardId(600),
            PlayerId(0),
            "Damage Watcher".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&watcher)
            .unwrap()
            .trigger_definitions
            .push({
                let mut trigger = TriggerDefinition::new(TriggerMode::DamageDoneOnceByController)
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Spell,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ));
                trigger.valid_source = Some(crate::types::ability::TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ));
                trigger.valid_target = Some(crate::types::ability::TargetFilter::Player);
                trigger
            });

        let attacker = create_creature(&mut state, PlayerId(0), "Double Striker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::DoubleStrike);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.stack.len(), 2);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CombatDamageDealtToPlayer { .. }))
                .count(),
            2
        );
    }
}
