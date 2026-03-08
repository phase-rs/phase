use crate::game::combat::{CombatState, DamageAssignment, DamageTarget};
use crate::game::sba;
use crate::game::triggers;
use crate::types::ability::TargetRef;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;

/// Resolve combat damage with first strike / double strike support.
/// If any creature in combat has FirstStrike or DoubleStrike, two damage sub-steps run.
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
            .map(|o| {
                o.has_keyword(&Keyword::FirstStrike) || o.has_keyword(&Keyword::DoubleStrike)
            })
            .unwrap_or(false)
    }) || combat
        .blocker_to_attacker
        .keys()
        .any(|blocker_id| {
            state
                .objects
                .get(blocker_id)
                .map(|o| {
                    o.has_keyword(&Keyword::FirstStrike) || o.has_keyword(&Keyword::DoubleStrike)
                })
                .unwrap_or(false)
        });

    if has_first_or_double {
        // First strike damage step
        first_strike_damage_step(state, events);

        // Mark first strike done
        if let Some(c) = &mut state.combat {
            c.first_strike_done = true;
        }

        // SBAs between damage steps
        sba::check_state_based_actions(state, events);
        triggers::process_triggers(state, events);

        // Regular damage step
        regular_damage_step(state, events);
    } else {
        // Single damage step
        regular_damage_step(state, events);
    }
}

/// First strike damage step: only FirstStrike and DoubleStrike creatures deal damage.
fn first_strike_damage_step(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let combat = match &state.combat {
        Some(c) => c.clone(),
        None => return,
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
        let assignments =
            assign_attacker_damage(state, attacker_info.object_id, &combat, power, has_deathtouch, has_trample);
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

    apply_combat_damage(state, &all_assignments, events);
}

/// Regular damage step: creatures WITHOUT FirstStrike (already dealt) + DoubleStrike creatures deal damage.
fn regular_damage_step(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let combat = match &state.combat {
        Some(c) => c.clone(),
        None => return,
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
        let assignments =
            assign_attacker_damage(state, attacker_info.object_id, &combat, power, has_deathtouch, has_trample);
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

    apply_combat_damage(state, &all_assignments, events);
}

/// Determine how an attacker assigns its damage.
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
                    // Single blocker with trample: assign lethal then excess to player
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
fn lethal_damage_needed(state: &GameState, object_id: ObjectId, source_has_deathtouch: bool) -> u32 {
    if source_has_deathtouch {
        // With deathtouch, 1 damage is lethal
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
fn apply_combat_damage(
    state: &mut GameState,
    assignments: &[(ObjectId, DamageAssignment)],
    events: &mut Vec<GameEvent>,
) {
    for (source_id, assignment) in assignments {
        let source_has_deathtouch = state
            .objects
            .get(source_id)
            .map(|o| o.has_keyword(&Keyword::Deathtouch))
            .unwrap_or(false);
        let source_has_lifelink = state
            .objects
            .get(source_id)
            .map(|o| o.has_keyword(&Keyword::Lifelink))
            .unwrap_or(false);
        let source_controller = state
            .objects
            .get(source_id)
            .map(|o| o.controller)
            .unwrap_or(crate::types::player::PlayerId(0));

        match &assignment.target {
            DamageTarget::Object(target_id) => {
                if let Some(target_obj) = state.objects.get_mut(target_id) {
                    target_obj.damage_marked += assignment.amount;
                    if source_has_deathtouch {
                        target_obj.dealt_deathtouch_damage = true;
                    }
                }
                events.push(GameEvent::DamageDealt {
                    source_id: *source_id,
                    target: TargetRef::Object(*target_id),
                    amount: assignment.amount,
                });
            }
            DamageTarget::Player(player_id) => {
                if let Some(player) = state.players.iter_mut().find(|p| p.id == *player_id) {
                    player.life -= assignment.amount as i32;
                }
                events.push(GameEvent::DamageDealt {
                    source_id: *source_id,
                    target: TargetRef::Player(*player_id),
                    amount: assignment.amount,
                });
                events.push(GameEvent::LifeChanged {
                    player_id: *player_id,
                    amount: -(assignment.amount as i32),
                });
            }
        }

        // Lifelink: source's controller gains life equal to damage dealt
        if source_has_lifelink && assignment.amount > 0 {
            if let Some(player) = state
                .players
                .iter_mut()
                .find(|p| p.id == source_controller)
            {
                player.life += assignment.amount as i32;
            }
            events.push(GameEvent::LifeChanged {
                player_id: source_controller,
                amount: assignment.amount as i32,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
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
        let mut combat = CombatState::default();
        combat.attackers = attackers
            .iter()
            .map(|&id| AttackerInfo {
                object_id: id,
                defending_player: PlayerId(1),
            })
            .collect();
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
}
