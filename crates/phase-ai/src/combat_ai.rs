use engine::game::combat::AttackTarget;
use engine::game::players;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::config::AiProfile;
use crate::eval::{evaluate_creature, threat_level};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CombatObjective {
    PushLethal,
    Stabilize,
    PreserveAdvantage,
    Race,
}

/// Choose which creatures to attack with and assign each to an opponent.
/// Returns `(ObjectId, AttackTarget)` pairs for per-creature targeting.
/// Strategy: evaluate threat per opponent, check for lethal on weakest,
/// then distribute remaining attackers toward highest-threat opponent.
pub fn choose_attackers_with_targets(
    state: &GameState,
    player: PlayerId,
) -> Vec<(ObjectId, AttackTarget)> {
    choose_attackers_with_targets_with_profile(state, player, &AiProfile::default())
}

pub fn choose_attackers_with_targets_with_profile(
    state: &GameState,
    player: PlayerId,
    profile: &AiProfile,
) -> Vec<(ObjectId, AttackTarget)> {
    let opponents = players::opponents(state, player);
    if opponents.is_empty() {
        return Vec::new();
    }

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
    let preferred_opponent = preferred_attack_opponent(state, player, &opponents, &candidates);
    // Collect blockers for the most likely attack target rather than the whole table.
    let opponent_blockers: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = state.objects.get(&id)?;
            if Some(obj.controller) == preferred_opponent
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && !obj.tapped
            {
                Some(id)
            } else {
                None
            }
        })
        .collect();
    let objective = determine_attack_objective(
        state,
        player,
        &opponents,
        &candidates,
        &opponent_blockers,
        profile,
    );

    // Determine which creatures should attack (same logic as before)
    let mut attacking_ids = Vec::new();
    for &id in &candidates {
        let obj = match state.objects.get(&id) {
            Some(o) => o,
            None => continue,
        };

        let my_value = evaluate_creature(state, id);
        let my_power = obj.power.unwrap_or(0);

        let has_evasion = obj.has_keyword(&Keyword::Flying)
            || obj.has_keyword(&Keyword::Menace)
            || obj.has_keyword(&Keyword::Shadow);

        if has_evasion || opponent_blockers.is_empty() {
            attacking_ids.push(id);
            continue;
        }

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
                let blocker_power = blocker.power.unwrap_or(0);
                (
                    bid,
                    evaluate_creature(state, bid),
                    blocker_toughness,
                    blocker_power,
                )
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        match best_blocker_value {
            None => attacking_ids.push(id),
            Some((_blocker_id, blocker_value, blocker_toughness, blocker_power)) => {
                let attacker_toughness = obj.toughness.unwrap_or(0);
                let kills_blocker = my_power >= blocker_toughness;
                let attacker_survives = attacker_toughness > blocker_power;
                // Free damage: attacker kills the blocker and lives to fight again
                let free_damage = kills_blocker && attacker_survives;
                // Favorable trade: attacker kills blocker and is worth less (trading up)
                let favorable_trade = kills_blocker && my_value <= blocker_value;
                if should_attack_given_objective(objective, free_damage, favorable_trade, profile) {
                    attacking_ids.push(id);
                }
            }
        }
    }

    // Single opponent: all attackers go to the same target
    if opponents.len() == 1 {
        let target = AttackTarget::Player(opponents[0]);
        return attacking_ids
            .into_iter()
            .map(|id| (id, target.clone()))
            .collect();
    }

    // Multi-opponent: assign attack targets
    assign_attack_targets(state, player, &opponents, attacking_ids)
}

fn preferred_attack_opponent(
    state: &GameState,
    player: PlayerId,
    opponents: &[PlayerId],
    candidate_attackers: &[ObjectId],
) -> Option<PlayerId> {
    if opponents.is_empty() {
        return None;
    }
    if opponents.len() == 1 {
        return Some(opponents[0]);
    }

    let total_attack_power = sum_power(state, candidate_attackers);
    let weakest = opponents
        .iter()
        .min_by_key(|&&opp| state.players[opp.0 as usize].life)
        .copied();
    if let Some(weakest) = weakest {
        let weak_life = state.players[weakest.0 as usize].life;
        if weak_life > 0 && total_attack_power >= weak_life {
            return Some(weakest);
        }
    }

    opponents.iter().copied().max_by(|&a, &b| {
        threat_level(state, player, a)
            .partial_cmp(&threat_level(state, player, b))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Assign each attacker to an opponent based on threat and lethal detection.
fn assign_attack_targets(
    state: &GameState,
    player: PlayerId,
    opponents: &[PlayerId],
    attacking_ids: Vec<ObjectId>,
) -> Vec<(ObjectId, AttackTarget)> {
    // Sort opponents by threat (descending)
    let mut threat_ranked: Vec<(PlayerId, f64)> = opponents
        .iter()
        .map(|&opp| (opp, threat_level(state, player, opp)))
        .collect();
    threat_ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let total_power: i32 = attacking_ids
        .iter()
        .filter_map(|&id| state.objects.get(&id))
        .map(|obj| obj.power.unwrap_or(0))
        .sum();

    // Check for alpha-strike: can we eliminate the weakest opponent?
    let weakest = opponents
        .iter()
        .min_by_key(|&&opp| state.players[opp.0 as usize].life)
        .copied();

    if let Some(weak_opp) = weakest {
        let weak_life = state.players[weak_opp.0 as usize].life;
        if weak_life > 0 && total_power >= weak_life {
            // Send enough to kill the weakest, rest to highest threat
            let target_weak = AttackTarget::Player(weak_opp);
            let primary_target = AttackTarget::Player(threat_ranked[0].0);
            let mut result = Vec::new();
            let mut allocated_power = 0;

            // Sort attackers by power (ascending) — send smallest first to just-kill threshold
            let mut sorted_attackers: Vec<(ObjectId, i32)> = attacking_ids
                .iter()
                .filter_map(|&id| state.objects.get(&id).map(|o| (id, o.power.unwrap_or(0))))
                .collect();
            sorted_attackers.sort_by_key(|&(_, p)| p);

            for (id, power) in sorted_attackers {
                if allocated_power < weak_life {
                    result.push((id, target_weak.clone()));
                    allocated_power += power;
                } else {
                    // If weakest IS the highest threat, keep sending there
                    let target = if weak_opp == threat_ranked[0].0 {
                        target_weak.clone()
                    } else {
                        primary_target.clone()
                    };
                    result.push((id, target));
                }
            }
            return result;
        }
    }

    // Default: send all to highest-threat opponent
    let primary = AttackTarget::Player(threat_ranked[0].0);
    attacking_ids
        .into_iter()
        .map(|id| (id, primary.clone()))
        .collect()
}

/// Backward-compatible wrapper: returns just attacker IDs (all targeting first opponent).
pub fn choose_attackers(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    choose_attackers_with_targets(state, player)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// Choose blocker assignments to minimize damage.
/// Assigns deathtouch creatures to highest-value attackers.
/// Prefers blocks where the blocker survives.
pub fn choose_blockers(
    state: &GameState,
    player: PlayerId,
    attacker_ids: &[ObjectId],
) -> Vec<(ObjectId, ObjectId)> {
    choose_blockers_with_profile(state, player, attacker_ids, &AiProfile::default())
}

pub fn choose_blockers_with_profile(
    state: &GameState,
    player: PlayerId,
    attacker_ids: &[ObjectId],
    profile: &AiProfile,
) -> Vec<(ObjectId, ObjectId)> {
    let mut assignments = Vec::new();
    let mut used_blockers = Vec::new();
    let objective = determine_block_objective(state, player, attacker_ids, profile);

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
            let attacker_power = attacker.power.unwrap_or(0);
            let p_life = state.players[player.0 as usize].life;
            // Chump block: sacrifice the blocker to prevent significant damage
            // when life total is threatened (attacker power >= 3 and life <= 3x that)
            let should_chump = priority == 0
                && attacker_power >= 3
                && matches!(objective, CombatObjective::Stabilize)
                && p_life <= attacker_power * 3;
            if priority > 0 || should_chump {
                assignments.push((blocker_id, attacker_id));
                used_blockers.push(blocker_id);
            }
        }
    }

    assignments
}

fn determine_attack_objective(
    state: &GameState,
    player: PlayerId,
    opponents: &[PlayerId],
    candidate_attackers: &[ObjectId],
    opponent_blockers: &[ObjectId],
    profile: &AiProfile,
) -> CombatObjective {
    let my_life = state.players[player.0 as usize].life;
    let min_opp_life = opponents
        .iter()
        .map(|&opp| state.players[opp.0 as usize].life)
        .min()
        .unwrap_or(20);
    let total_attack_power = sum_power(state, candidate_attackers);
    if min_opp_life > 0 && total_attack_power >= min_opp_life && opponent_blockers.is_empty() {
        return CombatObjective::PushLethal;
    }

    let my_board_power = battlefield_power(state, player);
    let opp_board_power: i32 = opponents
        .iter()
        .map(|&opp| battlefield_power(state, opp))
        .sum();

    if my_life as f64 <= opp_board_power.max(0) as f64 * profile.stabilize_bias {
        CombatObjective::Stabilize
    } else if my_board_power as f64
        >= opp_board_power as f64 * (1.0 - (profile.risk_tolerance * 0.2))
        && my_life >= min_opp_life
    {
        CombatObjective::PreserveAdvantage
    } else {
        CombatObjective::Race
    }
}

fn determine_block_objective(
    state: &GameState,
    player: PlayerId,
    attacker_ids: &[ObjectId],
    profile: &AiProfile,
) -> CombatObjective {
    let life = state.players[player.0 as usize].life;
    let incoming_power = sum_power(state, attacker_ids);
    if life as f64 <= incoming_power as f64 * profile.stabilize_bias {
        CombatObjective::Stabilize
    } else {
        CombatObjective::PreserveAdvantage
    }
}

fn should_attack_given_objective(
    objective: CombatObjective,
    free_damage: bool,
    favorable_trade: bool,
    profile: &AiProfile,
) -> bool {
    match objective {
        CombatObjective::PushLethal => true,
        CombatObjective::Stabilize => free_damage && profile.risk_tolerance < 0.8,
        CombatObjective::PreserveAdvantage => free_damage || favorable_trade,
        CombatObjective::Race => free_damage || favorable_trade || profile.risk_tolerance > 0.75,
    }
}

fn battlefield_power(state: &GameState, player: PlayerId) -> i32 {
    state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let object = state.objects.get(&id)?;
            if object.controller == player
                && object.card_types.core_types.contains(&CoreType::Creature)
            {
                Some(object.power.unwrap_or(0))
            } else {
                None
            }
        })
        .sum()
}

fn sum_power(state: &GameState, ids: &[ObjectId]) -> i32 {
    ids.iter()
        .filter_map(|&id| {
            state
                .objects
                .get(&id)
                .map(|object| object.power.unwrap_or(0))
        })
        .sum()
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
        .is_some_and(|etb| etb < state.turn_number)
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

    fn setup_multiplayer(player_count: u8) -> GameState {
        let mut state = GameState::new(
            engine::types::format::FormatConfig::free_for_all(),
            player_count,
            42,
        );
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
        assert!(
            attackers.contains(&flyer),
            "Flying creature should always attack"
        );
    }

    #[test]
    fn attacks_when_no_blockers() {
        let mut state = setup();
        let bear = add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);

        let attackers = choose_attackers(&state, PlayerId(0));
        assert!(
            attackers.contains(&bear),
            "Should attack with no blockers present"
        );
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
    fn lethal_objective_does_not_ignore_available_blockers() {
        let mut state = setup();
        state.players[1].life = 3;
        let attacker = add_creature(&mut state, PlayerId(0), "Bear", 3, 3, vec![]);
        add_creature(&mut state, PlayerId(1), "Wall", 0, 4, vec![]);

        let attackers = choose_attackers(&state, PlayerId(0));

        assert!(
            !attackers.contains(&attacker),
            "Should not alpha-strike into a blocker just because raw power equals life"
        );
    }

    #[test]
    fn deathtouch_blocker_assigned_to_biggest_threat() {
        let mut state = setup();
        let big = add_creature(
            &mut state,
            PlayerId(0),
            "Dragon",
            6,
            6,
            vec![Keyword::Flying],
        );
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
        let blocked_target = blockers.iter().find(|&&(b, _)| b == dt).map(|&(_, a)| a);
        assert_eq!(
            blocked_target,
            Some(big),
            "Deathtouch should block highest-value attacker"
        );
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
        assert!(
            blocker_ids.contains(&wall),
            "Wall should block since it survives"
        );
    }

    #[test]
    fn low_life_prefers_stabilizing_chump_block() {
        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Giant", 5, 5, vec![]);
        let chump = add_creature(&mut state, PlayerId(1), "Token", 1, 1, vec![]);
        state.players[1].life = 4;

        let blockers = choose_blockers(&state, PlayerId(1), &[attacker]);

        assert!(
            blockers.contains(&(chump, attacker)),
            "Low-life defender should chump to stabilize"
        );
    }

    #[test]
    fn stable_life_avoids_pointless_chump_block() {
        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Giant", 5, 5, vec![]);
        let chump = add_creature(&mut state, PlayerId(1), "Token", 1, 1, vec![]);
        state.players[1].life = 20;

        let blockers = choose_blockers(&state, PlayerId(1), &[attacker]);

        assert!(
            !blockers.contains(&(chump, attacker)),
            "Healthy defender should keep the chump blocker"
        );
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
        let id = add_creature(
            &mut state,
            PlayerId(0),
            "Wall",
            0,
            5,
            vec![Keyword::Defender],
        );
        assert!(!can_attack(&state, id));
    }

    // --- Multiplayer attack target tests ---

    #[test]
    fn three_player_attacks_highest_threat() {
        let mut state = setup_multiplayer(3);
        // Player 1 has strong board (high threat) but creatures are tapped (can't block)
        let d = add_creature(&mut state, PlayerId(1), "Dragon", 5, 5, vec![]);
        state.objects.get_mut(&d).unwrap().tapped = true;
        let a = add_creature(&mut state, PlayerId(1), "Angel", 4, 4, vec![]);
        state.objects.get_mut(&a).unwrap().tapped = true;
        // Player 0 has an attacker
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);

        let attacks = choose_attackers_with_targets(&state, PlayerId(0));
        assert!(!attacks.is_empty(), "Should have attackers");

        // All attacks should target player 1 (highest threat)
        for (_, target) in &attacks {
            assert_eq!(
                *target,
                AttackTarget::Player(PlayerId(1)),
                "Should attack highest-threat opponent"
            );
        }
    }

    #[test]
    fn three_player_splits_to_finish_weak_opponent() {
        let mut state = setup_multiplayer(3);
        // Player 1 has strong board, player 2 is nearly dead
        add_creature(&mut state, PlayerId(1), "Dragon", 5, 5, vec![]);
        state.players[2].life = 3; // Near death

        // Player 0 has multiple attackers with enough total power
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);
        add_creature(&mut state, PlayerId(0), "Bear2", 2, 2, vec![]);
        add_creature(&mut state, PlayerId(0), "Bear3", 3, 3, vec![]);

        let attacks = choose_attackers_with_targets(&state, PlayerId(0));
        assert!(attacks.len() >= 2, "Should have multiple attackers");

        // Should have some attacks targeting player 2 (weak opponent to finish off)
        let attacks_on_p2 = attacks
            .iter()
            .filter(|(_, t)| *t == AttackTarget::Player(PlayerId(2)))
            .count();
        assert!(
            attacks_on_p2 > 0,
            "Should allocate attackers to finish off weak opponent"
        );
    }

    #[test]
    fn generates_per_creature_attack_targets() {
        let mut state = setup_multiplayer(3);
        add_creature(&mut state, PlayerId(0), "A", 3, 3, vec![]);
        add_creature(&mut state, PlayerId(0), "B", 2, 2, vec![]);

        let attacks = choose_attackers_with_targets(&state, PlayerId(0));

        // Each attack should have a valid target
        for (obj_id, target) in &attacks {
            assert!(state.objects.contains_key(obj_id));
            match target {
                AttackTarget::Player(pid) => {
                    assert_ne!(*pid, PlayerId(0), "Cannot attack self");
                }
                AttackTarget::Planeswalker(_) => {} // Valid but unlikely here
            }
        }
    }

    #[test]
    fn two_player_backward_compat() {
        let mut state = setup();
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);

        let attacks = choose_attackers_with_targets(&state, PlayerId(0));
        assert!(!attacks.is_empty());
        // In 2-player, all attacks target player 1
        for (_, target) in &attacks {
            assert_eq!(*target, AttackTarget::Player(PlayerId(1)));
        }
    }
}
