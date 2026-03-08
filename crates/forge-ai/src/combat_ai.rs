use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::eval::evaluate_creature;

/// Choose which creatures to attack with.
/// Evaluates whether each attack is profitable based on creature values
/// and likely blocker assignments. Aggressive when ahead on life.
pub fn choose_attackers(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    let opponent = PlayerId(1 - player.0);
    let p_life = state.players[player.0 as usize].life;
    let o_life = state.players[opponent.0 as usize].life;
    let ahead_on_life = p_life > o_life;

    let candidates: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = state.objects.get(&id)?;
            if obj.controller == player && can_attack(state, id) {
                Some(id)
            } else {
                None
            }
        })
        .collect();

    let opponent_blockers: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = state.objects.get(&id)?;
            if obj.controller == opponent
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && !obj.tapped
            {
                Some(id)
            } else {
                None
            }
        })
        .collect();

    let mut attackers = Vec::new();

    for &id in &candidates {
        let obj = match state.objects.get(&id) {
            Some(o) => o,
            None => continue,
        };

        let my_value = evaluate_creature(state, id);
        let my_power = obj.power.unwrap_or(0);

        // Always attack with evasion creatures (flying, menace, shadow)
        let has_evasion = obj.has_keyword(&Keyword::Flying)
            || obj.has_keyword(&Keyword::Menace)
            || obj.has_keyword(&Keyword::Shadow);

        if has_evasion {
            attackers.push(id);
            continue;
        }

        // If no blockers exist, always attack
        if opponent_blockers.is_empty() {
            attackers.push(id);
            continue;
        }

        // Check if attacking is profitable
        let best_blocker_value = opponent_blockers
            .iter()
            .filter(|&&bid| {
                let blocker = match state.objects.get(&bid) {
                    Some(b) => b,
                    None => return false,
                };
                can_block_check(blocker, obj)
            })
            .map(|&bid| {
                let blocker = state.objects.get(&bid).unwrap();
                let blocker_toughness = blocker.toughness.unwrap_or(0);
                (bid, evaluate_creature(state, bid), blocker_toughness)
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        match best_blocker_value {
            None => {
                // No blocker can block us, safe to attack
                attackers.push(id);
            }
            Some((_blocker_id, blocker_value, blocker_toughness)) => {
                // Attack if our power kills the blocker and we're worth less or equal
                if my_power >= blocker_toughness && my_value <= blocker_value {
                    attackers.push(id);
                } else if ahead_on_life {
                    // When ahead, be more aggressive
                    attackers.push(id);
                }
                // Otherwise skip -- unprofitable attack
            }
        }
    }

    attackers
}

/// Choose blocker assignments to minimize damage.
/// Assigns deathtouch creatures to highest-value attackers.
/// Prefers blocks where the blocker survives.
pub fn choose_blockers(
    state: &GameState,
    player: PlayerId,
    attacker_ids: &[ObjectId],
) -> Vec<(ObjectId, ObjectId)> {
    let mut assignments = Vec::new();
    let mut used_blockers = Vec::new();

    // Collect available blockers
    let available_blockers: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = state.objects.get(&id)?;
            if obj.controller == player
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && !obj.tapped
            {
                Some(id)
            } else {
                None
            }
        })
        .collect();

    // Sort attackers by value (highest first) to prioritize blocking high-value threats
    let mut sorted_attackers: Vec<(ObjectId, f64)> = attacker_ids
        .iter()
        .map(|&id| (id, evaluate_creature(state, id)))
        .collect();
    sorted_attackers.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // First pass: assign deathtouch blockers to highest-value attackers
    for &(attacker_id, _) in &sorted_attackers {
        let attacker = match state.objects.get(&attacker_id) {
            Some(a) => a,
            None => continue,
        };

        if let Some(pos) = available_blockers.iter().position(|&bid| {
            if used_blockers.contains(&bid) {
                return false;
            }
            let blocker = match state.objects.get(&bid) {
                Some(b) => b,
                None => return false,
            };
            blocker.has_keyword(&Keyword::Deathtouch) && can_block_check(blocker, attacker)
        }) {
            let blocker_id = available_blockers[pos];
            assignments.push((blocker_id, attacker_id));
            used_blockers.push(blocker_id);
        }
    }

    // Second pass: assign remaining blockers where they'd survive
    for &(attacker_id, _) in &sorted_attackers {
        if assignments.iter().any(|&(_, a)| a == attacker_id) {
            continue; // Already blocked
        }

        let attacker = match state.objects.get(&attacker_id) {
            Some(a) => a,
            None => continue,
        };
        let attacker_power = attacker.power.unwrap_or(0);

        // Find a blocker that survives and can kill the attacker
        let best = available_blockers
            .iter()
            .filter(|&&bid| {
                !used_blockers.contains(&bid)
                    && state
                        .objects
                        .get(&bid)
                        .map(|b| can_block_check(b, attacker))
                        .unwrap_or(false)
            })
            .filter_map(|&bid| {
                let blocker = state.objects.get(&bid)?;
                let blocker_toughness = blocker.toughness.unwrap_or(0);
                let blocker_power = blocker.power.unwrap_or(0);
                let survives = blocker_toughness > attacker_power;
                let kills = blocker_power >= attacker.toughness.unwrap_or(0);
                // Prefer: survives and kills > survives > kills > neither
                let priority = (survives as u8) * 2 + (kills as u8);
                Some((bid, priority, evaluate_creature(state, bid)))
            })
            .max_by(|a, b| {
                a.1.cmp(&b.1)
                    .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
            });

        if let Some((blocker_id, priority, _)) = best {
            // Only block if the blocker survives or the trade is worthwhile
            if priority > 0 {
                assignments.push((blocker_id, attacker_id));
                used_blockers.push(blocker_id);
            }
        }
    }

    assignments
}

/// Check if a creature can attack (not tapped, no defender, no summoning sickness).
fn can_attack(state: &GameState, obj_id: ObjectId) -> bool {
    let obj = match state.objects.get(&obj_id) {
        Some(o) => o,
        None => return false,
    };

    if obj.zone != Zone::Battlefield {
        return false;
    }
    if !obj.card_types.core_types.contains(&CoreType::Creature) {
        return false;
    }
    if obj.tapped {
        return false;
    }
    if obj.has_keyword(&Keyword::Defender) {
        return false;
    }

    // Summoning sickness check
    if obj.has_keyword(&Keyword::Haste) {
        return true;
    }
    obj.entered_battlefield_turn
        .map_or(false, |etb| etb < state.turn_number)
}

/// Check if a blocker can legally block an attacker (flying/reach, shadow checks).
fn can_block_check(
    blocker: &engine::game::game_object::GameObject,
    attacker: &engine::game::game_object::GameObject,
) -> bool {
    // Flying check
    if attacker.has_keyword(&Keyword::Flying)
        && !blocker.has_keyword(&Keyword::Flying)
        && !blocker.has_keyword(&Keyword::Reach)
    {
        return false;
    }

    // Shadow check
    let attacker_shadow = attacker.has_keyword(&Keyword::Shadow);
    let blocker_shadow = blocker.has_keyword(&Keyword::Shadow);
    if attacker_shadow != blocker_shadow {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::identifiers::CardId;

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state
    }

    fn add_creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
        keywords: Vec<Keyword>,
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
        obj.keywords = keywords;
        obj.entered_battlefield_turn = Some(1);
        id
    }

    #[test]
    fn attacks_with_evasion_creatures() {
        let mut state = setup();
        let flyer = add_creature(&mut state, PlayerId(0), "Bird", 2, 2, vec![Keyword::Flying]);
        add_creature(&mut state, PlayerId(1), "Bear", 2, 2, vec![]);

        let attackers = choose_attackers(&state, PlayerId(0));
        assert!(attackers.contains(&flyer), "Flying creature should always attack");
    }

    #[test]
    fn attacks_when_no_blockers() {
        let mut state = setup();
        let bear = add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);

        let attackers = choose_attackers(&state, PlayerId(0));
        assert!(attackers.contains(&bear), "Should attack with no blockers present");
    }

    #[test]
    fn skips_unprofitable_attack() {
        let mut state = setup();
        // Small attacker vs big blocker, equal life totals
        let small = add_creature(&mut state, PlayerId(0), "Squirrel", 1, 1, vec![]);
        add_creature(&mut state, PlayerId(1), "Giant", 5, 5, vec![]);

        let attackers = choose_attackers(&state, PlayerId(0));
        assert!(
            !attackers.contains(&small),
            "Should skip 1/1 into 5/5 when life is equal"
        );
    }

    #[test]
    fn deathtouch_blocker_assigned_to_biggest_threat() {
        let mut state = setup();
        let big = add_creature(&mut state, PlayerId(0), "Dragon", 6, 6, vec![Keyword::Flying]);
        let small = add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);
        let dt = add_creature(
            &mut state,
            PlayerId(1),
            "Snake",
            1,
            1,
            vec![Keyword::Deathtouch, Keyword::Flying],
        );

        let blockers = choose_blockers(&state, PlayerId(1), &[big, small]);

        // Deathtouch blocker should be assigned to the dragon (highest value)
        let blocked_target = blockers
            .iter()
            .find(|&&(b, _)| b == dt)
            .map(|&(_, a)| a);
        assert_eq!(blocked_target, Some(big), "Deathtouch should block highest-value attacker");
    }

    #[test]
    fn blocker_prefers_surviving_block() {
        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);
        let _small = add_creature(&mut state, PlayerId(1), "Squirrel", 1, 1, vec![]);
        let wall = add_creature(&mut state, PlayerId(1), "Wall", 0, 4, vec![]);

        let blockers = choose_blockers(&state, PlayerId(1), &[attacker]);

        // Wall should block (survives), squirrel should not (dies for nothing)
        let blocker_ids: Vec<_> = blockers.iter().map(|&(b, _)| b).collect();
        assert!(blocker_ids.contains(&wall), "Wall should block since it survives");
    }

    #[test]
    fn can_attack_respects_summoning_sickness() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);
        state.objects.get_mut(&id).unwrap().entered_battlefield_turn = Some(2); // this turn
        assert!(!can_attack(&state, id));
    }

    #[test]
    fn can_attack_haste_ignores_sickness() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Hasty", 3, 1, vec![Keyword::Haste]);
        state.objects.get_mut(&id).unwrap().entered_battlefield_turn = Some(2); // this turn
        assert!(can_attack(&state, id));
    }

    #[test]
    fn defender_cannot_attack() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Wall", 0, 5, vec![Keyword::Defender]);
        assert!(!can_attack(&state, id));
    }
}
