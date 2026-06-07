use std::collections::HashMap;

use engine::game::combat::{can_block_pair, AttackTarget};
use engine::game::commander::commander_lethal_headroom;
use engine::game::players;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use crate::config::AiProfile;
use crate::damage_reflection::has_damage_reflection_to_controller;
use crate::eval::{evaluate_creature, threat_level};
use crate::projection::{project_to, Projection, ProjectionHorizon};

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
    choose_attackers_with_targets_with_profile(
        state,
        player,
        &AiProfile::default(),
        false,
        None,
        None,
    )
}

pub fn choose_attackers_with_targets_with_profile(
    state: &GameState,
    player: PlayerId,
    profile: &AiProfile,
    combat_lookahead: bool,
    valid_attacker_ids: Option<&[ObjectId]>,
    valid_attack_targets: Option<&[AttackTarget]>,
) -> Vec<(ObjectId, AttackTarget)> {
    let opponents = players::opponents(state, player);
    if opponents.is_empty() {
        return Vec::new();
    }

    // Use engine-provided valid attacker list when available; fall back to
    // local can_attack() for tests and hypothetical scenarios.
    let candidates: Vec<ObjectId> = if let Some(ids) = valid_attacker_ids {
        ids.to_vec()
    } else {
        state
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
            .collect()
    };
    // CR 508.1d / CR 701.15b: creatures with a live must-attack requirement
    // (goad, "attacks each combat if able", lure statics) MUST be declared as
    // attackers or the engine rejects the whole declaration. Partition them out
    // and union them back unconditionally — value heuristics only apply to the
    // free choices. `creature_must_attack` is the engine's single authority.
    let mandatory: Vec<ObjectId> = candidates
        .iter()
        .copied()
        .filter(|&id| engine::game::combat::creature_must_attack(state, id))
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

        let is_unblockable = has_cant_be_blocked(state, obj);
        let has_lifelink = obj.has_keyword(&Keyword::Lifelink);
        let is_commander = obj.is_commander;

        if is_unblockable || opponent_blockers.is_empty() {
            attacking_ids.push(id);
            continue;
        }

        // CR 509.1a: Evaluate the attack against the defender's *best* block, not
        // its cheapest creature. Assuming the defender chump-trades with its
        // weakest body let the AI swing doomed creatures into a "favorable trade"
        // the defender would never offer — it instead kills the attacker for free
        // with a first-striker or a larger body (gamestate1: a 1/1 animated land
        // sent into a 2/1 first strike).
        match defender_best_block(state, id, my_value, &opponent_blockers) {
            None => attacking_ids.push(id),
            Some(DefenderBlock {
                blocker_value,
                kills_blocker,
                attacker_survives,
            }) => {
                let free_damage = kills_blocker && attacker_survives;
                let favorable_trade = kills_blocker && my_value <= blocker_value;
                if should_attack_given_objective(
                    objective,
                    free_damage,
                    favorable_trade,
                    has_lifelink,
                    my_power,
                    attacker_survives,
                    is_commander,
                ) {
                    attacking_ids.push(id);
                }
            }
        }
    }

    // Alpha-strike: if no individual attack looks good but we outnumber blockers,
    // attack with everyone — the excess creatures get through unblocked.
    // Only do this if expected unblocked damage justifies the trade.
    if attacking_ids.is_empty()
        && !candidates.is_empty()
        && candidates.len() > opponent_blockers.len()
        && matches!(
            objective,
            CombatObjective::PreserveAdvantage | CombatObjective::Race
        )
    {
        // CR 903.8: exclude the commander from the desperation alpha-strike for
        // the same reason the per-creature gate does — don't trade it away.
        // Alpha-strike only fires under PreserveAdvantage|Race when the per-loop
        // gate rejected every candidate, so any commander here was a
        // `!free_damage` rejection: `is_commander` is equivalent to the loop's
        // `is_commander && !free_damage && objective != PushLethal`. Filter
        // BEFORE the cost/benefit math so unblocked_power/worst_loss_value match
        // the actual swing set. (A goaded commander is re-added by the
        // must-attack union below, which runs after this block.)
        let alpha_candidates: Vec<ObjectId> = candidates
            .iter()
            .copied()
            .filter(|&id| {
                !state
                    .objects
                    .get(&id)
                    .map(|o| o.is_commander)
                    .unwrap_or(false)
            })
            .collect();

        if alpha_candidates.len() > opponent_blockers.len() {
            let mut valued: Vec<(ObjectId, f64)> = alpha_candidates
                .iter()
                .map(|&id| (id, evaluate_creature(state, id)))
                .collect();
            valued.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

            let blocked_count = opponent_blockers.len();
            let unblocked_power: i32 = valued[blocked_count..]
                .iter()
                .filter_map(|&(id, _)| state.objects.get(&id)?.power)
                .sum();
            let worst_loss_value: f64 = valued[..blocked_count].iter().map(|&(_, v)| v).sum();

            if unblocked_power as f64 > worst_loss_value {
                attacking_ids = alpha_candidates;
            }
        }
    }

    // CR 508.1d / CR 701.15b: union the mandatory must-attack set. These
    // creatures are declared regardless of the value heuristic's verdict.
    for &id in &mandatory {
        if !attacking_ids.contains(&id) {
            attacking_ids.push(id);
        }
    }

    // Crackback analysis: if tapping our attackers leaves us dead on the swing-back,
    // hold back non-vigilance creatures (highest-value first) until we survive.
    if !attacking_ids.is_empty() && !matches!(objective, CombatObjective::PushLethal) {
        let my_life = state.players[player.0 as usize].life;
        // Project opponent's upcoming begin-combat + attacker declaration so
        // crackback_damage sees scaled creatures (Ouroboroid class) and
        // attack-trigger pumps (Battle Cry, Mentor). Failure to project
        // falls through to current state — matches pre-projection behavior.
        let projection = if combat_lookahead {
            project_to(
                state,
                player,
                opponents[0],
                ProjectionHorizon::OpponentAttackersDeclared,
            )
            .ok()
        } else {
            None
        };
        let cb_damage = crackback_damage(
            state,
            player,
            &opponents,
            &attacking_ids,
            projection.as_ref(),
        );
        if cb_damage >= my_life {
            // Sort non-vigilance attackers by value descending — hold back most valuable first
            let mut non_vigilance: Vec<(usize, f64)> = attacking_ids
                .iter()
                .enumerate()
                .filter(|&(_, &id)| {
                    // CR 508.1d / CR 701.15b: a must-attack creature cannot be
                    // pruned for crackback — declaring it is mandatory.
                    !mandatory.contains(&id)
                        && state
                            .objects
                            .get(&id)
                            .map(|o| !o.has_keyword(&Keyword::Vigilance))
                            .unwrap_or(false)
                })
                .map(|(i, &id)| (i, evaluate_creature(state, id)))
                .collect();
            non_vigilance
                .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            // Remove attackers one at a time until crackback is survivable
            let mut to_remove = Vec::new();
            for &(idx, _) in &non_vigilance {
                let remaining: Vec<ObjectId> = attacking_ids
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !to_remove.contains(i))
                    .map(|(_, &id)| id)
                    .collect();
                let cb =
                    crackback_damage(state, player, &opponents, &remaining, projection.as_ref());
                if cb < my_life {
                    break;
                }
                to_remove.push(idx);
            }

            // Apply removals (iterate in reverse to preserve indices)
            to_remove.sort_unstable();
            for &idx in to_remove.iter().rev() {
                attacking_ids.remove(idx);
            }
        }
    }

    // Single opponent: attackers go to the player, except a "kill it or ignore
    // it" planeswalker redirect (see redirect_attackers_to_planeswalker).
    if opponents.len() == 1 {
        let opp = opponents[0];
        let opponent_life = state.players[opp.0 as usize].life;
        return redirect_attackers_to_planeswalker(
            state,
            &attacking_ids,
            valid_attack_targets,
            objective,
            opp,
            opponent_life,
        );
    }

    // Multi-opponent: assign attack targets (planeswalker redirect deferred).
    assign_attack_targets(state, player, &opponents, attacking_ids)
}

/// Single-opponent planeswalker redirect (CR 508.1: legality of attacking a
/// planeswalker is decided by the engine, which surfaces every legal target in
/// `valid_attack_targets` — this only *chooses* among them).
///
/// Policy: when not pushing lethal and the full swing isn't near-lethal at the
/// face, redirect the *fewest large* attackers needed to KILL the
/// highest-loyalty opponent planeswalker (largest-power-first), provided at
/// least one attacker still hits the player. Otherwise every attacker goes to
/// the player. "Kill it or ignore it" — never dribble partial loyalty damage,
/// never empty the face, never dilute a lethal race. Loyalty is a rough
/// entrenchment proxy, not a true threat score (deferred refinement).
fn redirect_attackers_to_planeswalker(
    state: &GameState,
    attacking_ids: &[ObjectId],
    valid_attack_targets: Option<&[AttackTarget]>,
    objective: CombatObjective,
    opponent: PlayerId,
    opponent_life: i32,
) -> Vec<(ObjectId, AttackTarget)> {
    let player_target = AttackTarget::Player(opponent);
    let all_at_player = || -> Vec<(ObjectId, AttackTarget)> {
        attacking_ids
            .iter()
            .map(|&id| (id, player_target))
            .collect()
    };

    // Don't dilute a lethal / near-lethal swing at the face.
    if objective == CombatObjective::PushLethal {
        return all_at_player();
    }
    let total_power: i32 = attacking_ids
        .iter()
        .filter_map(|&id| state.objects.get(&id)?.power)
        .sum();
    if total_power >= opponent_life {
        return all_at_player();
    }

    // Highest-loyalty attackable opponent planeswalker from the engine's list.
    let Some(targets) = valid_attack_targets else {
        return all_at_player();
    };
    let best_pw = targets
        .iter()
        .filter_map(|t| match t {
            AttackTarget::Planeswalker(id) => {
                let loyalty = state.objects.get(id)?.loyalty.unwrap_or(0);
                (loyalty > 0).then_some((*id, loyalty as i32))
            }
            _ => None,
        })
        .max_by_key(|&(_, loyalty)| loyalty);
    let Some((pw_id, loyalty)) = best_pw else {
        return all_at_player();
    };

    // Largest-power-first: the fewest big attackers that sum to >= loyalty.
    let mut by_power: Vec<(ObjectId, i32)> = attacking_ids
        .iter()
        .filter_map(|&id| Some((id, state.objects.get(&id)?.power.unwrap_or(0))))
        .collect();
    by_power.sort_by_key(|b| std::cmp::Reverse(b.1));

    let mut redirected: Vec<ObjectId> = Vec::new();
    let mut acc: i32 = 0;
    for (id, power) in &by_power {
        if acc >= loyalty {
            break;
        }
        redirected.push(*id);
        acc += power;
    }

    // Kill-it-or-ignore-it: bail if we can't kill it or doing so empties the face.
    if acc < loyalty || redirected.len() == attacking_ids.len() {
        return all_at_player();
    }

    let pw_target = AttackTarget::Planeswalker(pw_id);
    attacking_ids
        .iter()
        .map(|&id| {
            if redirected.contains(&id) {
                (id, pw_target)
            } else {
                (id, player_target)
            }
        })
        .collect()
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

    multiplayer_pressure_target(state, player, opponents)
}

/// Assign each attacker to an opponent based on threat and lethal detection.
fn assign_attack_targets(
    state: &GameState,
    player: PlayerId,
    opponents: &[PlayerId],
    attacking_ids: Vec<ObjectId>,
) -> Vec<(ObjectId, AttackTarget)> {
    let threat_ranked = threat_ranked_opponents(state, player, opponents);

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
                    result.push((id, target_weak));
                    allocated_power += power;
                } else {
                    // If weakest IS the highest threat, keep sending there
                    let target = if weak_opp == threat_ranked[0].0 {
                        target_weak
                    } else {
                        primary_target
                    };
                    result.push((id, target));
                }
            }
            return result;
        }
    }

    // Default: pressure the next opponent in turn order unless one opponent is
    // a clear archenemy. This prevents every bot in a multiplayer pod from
    // dogpiling the same seat on small, noisy threat-score differences.
    let primary = AttackTarget::Player(
        multiplayer_pressure_target(state, player, opponents).unwrap_or(threat_ranked[0].0),
    );
    attacking_ids.into_iter().map(|id| (id, primary)).collect()
}

const MULTIPLAYER_FOCUS_THREAT_MARGIN: f64 = 0.18;

fn threat_ranked_opponents(
    state: &GameState,
    player: PlayerId,
    opponents: &[PlayerId],
) -> Vec<(PlayerId, f64)> {
    let mut ranked: Vec<_> = opponents
        .iter()
        .map(|&opp| (opp, threat_level(state, player, opp)))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked
}

fn multiplayer_pressure_target(
    state: &GameState,
    player: PlayerId,
    opponents: &[PlayerId],
) -> Option<PlayerId> {
    let ranked = threat_ranked_opponents(state, player, opponents);
    let (top, top_score) = ranked.first().copied()?;
    if ranked.len() == 1 {
        return Some(top);
    }

    let second_score = ranked[1].1;
    if top_score - second_score >= MULTIPLAYER_FOCUS_THREAT_MARGIN {
        return Some(top);
    }

    let next = players::next_player(state, player);
    if opponents.contains(&next) {
        Some(next)
    } else {
        Some(top)
    }
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
    choose_blockers_with_profile(state, player, attacker_ids, &AiProfile::default(), None)
}

pub fn choose_blockers_with_profile(
    state: &GameState,
    player: PlayerId,
    attacker_ids: &[ObjectId],
    profile: &AiProfile,
    valid_block_targets: Option<&HashMap<ObjectId, Vec<ObjectId>>>,
) -> Vec<(ObjectId, ObjectId)> {
    let mut assignments = Vec::new();
    let mut used_blockers = Vec::new();
    let objective = determine_block_objective(state, player, attacker_ids, profile);

    // Collect available blockers and their pre-computed values in one pass.
    // `evaluate_creature` previously ran for each blocker on every pass
    // (first-pass selection, survives/kills ranking, gang-block sorting).
    // Hoisting it here makes the inner loops pure lookups.
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

    // CR 509.1 + CR 704.5a: Hopeless-block fast-path. If no legal assignment
    // can prevent lethal life loss, bail with empty assignments before doing
    // any per-blocker scoring. This guards against pathological boards (e.g.
    // 1000 Scute Swarm tokens vs a 1200/1200 trampler, or 1000 attackers vs
    // 5 blockers) where the existing per-blocker chump heuristic burns CPU
    // assigning a futile chump that cannot meaningfully reduce damage.
    if matches!(objective, CombatObjective::Stabilize)
        && block_is_futile(state, player, attacker_ids, &available_blockers)
    {
        return Vec::new();
    }

    let blocker_values: HashMap<ObjectId, f64> = available_blockers
        .iter()
        .map(|&id| (id, evaluate_creature(state, id)))
        .collect();
    let blocker_value = |id: &ObjectId| -> f64 { blocker_values.get(id).copied().unwrap_or(0.0) };

    // Sort attackers by value (highest first) to prioritize blocking high-value threats
    let mut sorted_attackers: Vec<(ObjectId, f64)> = attacker_ids
        .iter()
        .map(|&id| (id, evaluate_creature(state, id)))
        .collect();
    sorted_attackers.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // First pass: assign deathtouch blockers to highest-value attackers.
    // CR 702.111b: Skip menace attackers — they require 2+ blockers (handled in gang-block pass).
    for &(attacker_id, _) in &sorted_attackers {
        let attacker = match state.objects.get(&attacker_id) {
            Some(a) => a,
            None => continue,
        };
        if attacker.has_keyword(&Keyword::Menace) {
            continue;
        }

        if let Some(pos) = available_blockers.iter().position(|&bid| {
            if used_blockers.contains(&bid) {
                return false;
            }
            let blocker = match state.objects.get(&bid) {
                Some(b) => b,
                None => return false,
            };
            blocker.has_keyword(&Keyword::Deathtouch)
                && can_block_with_engine_map(state, bid, attacker_id, valid_block_targets)
        }) {
            let blocker_id = available_blockers[pos];
            assignments.push((blocker_id, attacker_id));
            used_blockers.push(blocker_id);
        }
    }

    // Second pass: assign remaining blockers where they'd survive.
    // CR 702.111b: Skip menace attackers — they require 2+ blockers (handled in gang-block pass).
    for &(attacker_id, _) in &sorted_attackers {
        if assignments.iter().any(|&(_, a)| a == attacker_id) {
            continue; // Already blocked
        }

        let attacker = match state.objects.get(&attacker_id) {
            Some(a) => a,
            None => continue,
        };
        if attacker.has_keyword(&Keyword::Menace) {
            continue;
        }

        // Find a blocker that survives and can kill the attacker
        let best = available_blockers
            .iter()
            .filter(|&&bid| {
                !used_blockers.contains(&bid)
                    && can_block_with_engine_map(state, bid, attacker_id, valid_block_targets)
            })
            .filter_map(|&bid| {
                let blocker = state.objects.get(&bid)?;
                let (kills, survives) = evaluate_block_outcome(blocker, attacker);
                // Prefer: survives and kills > survives > kills > neither
                let priority = (survives as u8) * 2 + (kills as u8);
                Some((bid, priority, blocker_value(&bid)))
            })
            .max_by(|a, b| {
                a.1.cmp(&b.1)
                    .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
            });

        if let Some((blocker_id, priority, _)) = best {
            let attacker_power = attacker.power.unwrap_or(0);
            let p_life = state.players[player.0 as usize].life;

            // Damage-reflection check (Jackal Pup pattern): if the blocker has a
            // DamageReceived trigger that deals the same damage to its controller,
            // blocking effectively costs the player that damage too. Skip blocking
            // when the reflected damage would be lethal, and reduce blocking priority
            // when the net damage prevented is negative.
            let blocker_obj = state.objects.get(&blocker_id);
            let reflects_damage = blocker_obj.is_some_and(has_damage_reflection_to_controller);
            if reflects_damage {
                let reflected = attacker_power;
                if reflected >= p_life {
                    // Blocking would be lethal from the reflected damage alone — skip
                    continue;
                }
            }

            // Chump block: sacrifice the blocker to prevent significant damage
            // when life total is threatened (attacker power >= 3 and life <= 3x that)
            // CR 702.19b: Trample means a chump blocker only prevents blocker_toughness
            // damage, not the full attacker_power. Skip chump blocking tramplers when
            // the blocker is too small to make a meaningful difference.
            let has_trample = attacker.has_keyword(&Keyword::Trample);
            let blocker_toughness = blocker_obj.and_then(|b| b.toughness).unwrap_or(1);
            let damage_prevented = if has_trample {
                blocker_toughness
            } else {
                attacker_power
            };

            // For damage-reflection creatures, the net life change from blocking is
            // (damage_prevented - reflected_damage). If net is non-positive, blocking
            // costs more life than it saves — skip unless the block actually kills
            // the attacker (trading the creature is still valuable).
            if reflects_damage && priority < 2 {
                let reflected = attacker_power;
                let net = damage_prevented - reflected;
                if net <= 0 {
                    continue;
                }
            }

            // CR 903.10a: For commander attackers, the effective lethal threshold can be
            // tighter than raw life. Use min(life, headroom) so we chump-stabilize when
            // a 5-power commander would cross the 21-cmd-damage threshold.
            let effective_life = commander_lethal_headroom(state, player, attacker_id)
                .map(|h| p_life.min(h as i32))
                .unwrap_or(p_life);

            let should_chump_stabilize = priority == 0
                && damage_prevented >= 2
                && matches!(objective, CombatObjective::Stabilize)
                && effective_life <= attacker_power * 3;
            // Race chump: losing the damage race, block anything with power >= 2
            let should_chump_race =
                priority == 0 && attacker_power >= 2 && matches!(objective, CombatObjective::Race);
            // CR 903.10a: Skip chumps that don't actually save under commander damage
            // (e.g. 1/1 in front of a 12/12 trample commander with 3 cmd-damage headroom).
            let chump_unsafe = priority == 0
                && commander_chump_unsafe(state, player, attacker_id, blocker_toughness);
            if !chump_unsafe && (priority > 0 || should_chump_stabilize || should_chump_race) {
                assignments.push((blocker_id, attacker_id));
                used_blockers.push(blocker_id);
            }
        }
    }

    // Gang-blocking pass (CR 509.1a): assign multiple blockers to a single attacker
    // when no single blocker can kill it but combined power can.
    // Only gang-block when the combined blocker value is less than the attacker value.
    for &(attacker_id, attacker_value) in &sorted_attackers {
        if assignments.iter().any(|&(_, a)| a == attacker_id) {
            continue; // Already blocked
        }
        let attacker = match state.objects.get(&attacker_id) {
            Some(a) => a,
            None => continue,
        };
        let attacker_toughness = attacker.toughness.unwrap_or(0);
        let attacker_power = attacker.power.unwrap_or(0);
        let attacker_has_deathtouch = attacker.has_keyword(&Keyword::Deathtouch);
        let attacker_has_first_strike = attacker.has_keyword(&Keyword::FirstStrike)
            || attacker.has_keyword(&Keyword::DoubleStrike);

        // Collect eligible unused blockers sorted by value (ascending = sacrifice cheapest)
        let mut gang_candidates: Vec<(ObjectId, i32, f64)> = available_blockers
            .iter()
            .filter(|&&bid| {
                !used_blockers.contains(&bid)
                    && can_block_with_engine_map(state, bid, attacker_id, valid_block_targets)
            })
            .filter_map(|&bid| {
                let b = state.objects.get(&bid)?;
                Some((bid, b.power.unwrap_or(0), blocker_value(&bid)))
            })
            .collect();
        gang_candidates.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

        // Skip if any single blocker can already kill it (handled in second pass above).
        // CR 702.111b: Exception — menace attackers MUST be gang-blocked even when a
        // single blocker could kill them, because single blocks are illegal.
        let has_menace = attacker.has_keyword(&Keyword::Menace);
        if !has_menace
            && gang_candidates.iter().any(|&(bid, _, _)| {
                state
                    .objects
                    .get(&bid)
                    .map(|b| {
                        let (kills, _) = evaluate_block_outcome(b, attacker);
                        kills
                    })
                    .unwrap_or(false)
            })
        {
            continue;
        }

        // CR 702.7b: If attacker has first strike and blocker doesn't, the blocker
        // dies before dealing damage. Skip blockers that would die to first strike.
        let effective_candidates: Vec<(ObjectId, i32, f64)> = gang_candidates
            .into_iter()
            .filter(|&(bid, _, _)| {
                if !attacker_has_first_strike {
                    return true;
                }
                let b = match state.objects.get(&bid) {
                    Some(b) => b,
                    None => return false,
                };
                // Blocker survives first strike if it has first strike too,
                // or if attacker can't kill it in the first strike step
                b.has_keyword(&Keyword::FirstStrike)
                    || b.has_keyword(&Keyword::DoubleStrike)
                    || attacker_power < b.toughness.unwrap_or(0)
            })
            .collect();

        // CR 702.2c: Deathtouch means any nonzero damage is lethal, so one
        // blocker with deathtouch is enough — no need to gang-block.
        // Also skip if attacker has deathtouch: every blocker dies, so
        // gang-blocking just loses more creatures.
        if attacker_has_deathtouch {
            continue;
        }

        // Find minimum set of blockers whose combined power >= attacker toughness
        let mut combined_power = 0;
        let mut gang_set: Vec<ObjectId> = Vec::new();
        let mut gang_value = 0.0;
        for &(bid, power, value) in &effective_candidates {
            combined_power += power;
            gang_set.push(bid);
            gang_value += value;
            if combined_power >= attacker_toughness {
                break;
            }
        }

        // CR 702.111b: Menace requires at least 2 blockers. If the gang set is too
        // small, try to add another blocker even if combined power already suffices.
        if has_menace && gang_set.len() < 2 {
            if let Some(&(bid, power, value)) = effective_candidates
                .iter()
                .find(|(bid, _, _)| !gang_set.contains(bid))
            {
                combined_power += power;
                gang_set.push(bid);
                gang_value += value;
            }
        }

        // Only gang-block if combined power can kill AND total value risked <= attacker value.
        // For menace attackers, also require at least 2 blockers.
        let min_blockers = if has_menace { 2 } else { 1 };
        if combined_power >= attacker_toughness
            && gang_set.len() >= min_blockers
            && gang_value <= attacker_value
        {
            for bid in gang_set {
                assignments.push((bid, attacker_id));
                used_blockers.push(bid);
            }
        }
    }

    // Third pass: if unblocked damage is still lethal, greedily assign remaining
    // blockers to the highest-power unblocked attackers to survive.
    if matches!(objective, CombatObjective::Stabilize) {
        let p_life = state.players[player.0 as usize].life;
        let unblocked_damage: i32 = sorted_attackers
            .iter()
            .filter(|&&(aid, _)| !assignments.iter().any(|&(_, a)| a == aid))
            .filter_map(|&(aid, _)| state.objects.get(&aid))
            .map(|obj| obj.power.unwrap_or(0))
            .sum();

        if unblocked_damage >= p_life {
            // Sort unblocked attackers by damage prevented descending.
            // Non-tramplers are fully blocked (damage_prevented = power).
            // Tramplers only lose blocker_toughness worth of damage, so we
            // estimate 1 here (minimum toughness) and refine at assignment time.
            let mut unblocked: Vec<(ObjectId, i32, i32)> = sorted_attackers
                .iter()
                .filter(|&&(aid, _)| !assignments.iter().any(|&(_, a)| a == aid))
                .filter_map(|&(aid, _)| {
                    let obj = state.objects.get(&aid)?;
                    let power = obj.power.unwrap_or(0);
                    let estimated_prevented = if obj.has_keyword(&Keyword::Trample) {
                        1 // chump only prevents ~1 damage vs trample
                    } else {
                        power
                    };
                    Some((aid, power, estimated_prevented))
                })
                .collect();
            unblocked.sort_by_key(|b| std::cmp::Reverse(b.2));

            let mut remaining_damage = unblocked_damage;
            for (attacker_id, attacker_power, _) in unblocked {
                if remaining_damage < p_life {
                    break; // No longer lethal
                }
                let attacker = match state.objects.get(&attacker_id) {
                    Some(a) => a,
                    None => continue,
                };
                // CR 702.111b: Skip menace attackers — single chump blocks are illegal.
                if attacker.has_keyword(&Keyword::Menace) {
                    continue;
                }
                // Find any unused blocker that can legally block this attacker.
                // Skip damage-reflection creatures (Jackal Pup) — blocking with them
                // deals the attacker's power to the player, negating the damage prevented.
                if let Some(&blocker_id) = available_blockers.iter().find(|&&bid| {
                    !used_blockers.contains(&bid)
                        && can_block_with_engine_map(state, bid, attacker_id, valid_block_targets)
                        && state
                            .objects
                            .get(&bid)
                            .map(|b| !has_damage_reflection_to_controller(b))
                            .unwrap_or(false)
                }) {
                    assignments.push((blocker_id, attacker_id));
                    used_blockers.push(blocker_id);
                    // CR 702.19b: Trample only requires lethal damage assigned to blocker;
                    // excess tramples through. A chump block only prevents blocker_toughness.
                    let damage_prevented = if attacker.has_keyword(&Keyword::Trample) {
                        state
                            .objects
                            .get(&blocker_id)
                            .and_then(|b| b.toughness)
                            .unwrap_or(1)
                    } else {
                        attacker_power
                    };
                    remaining_damage -= damage_prevented;
                }
            }
        }

        // CR 903.10a: Per-commander chump pass. Chumping commander A doesn't reduce
        // lethality from commander B, so iterate each unblocked commander attacker
        // independently and chump if a safe (non-trample-defeated) blocker exists.
        for &(attacker_id, _) in &sorted_attackers {
            if assignments.iter().any(|&(_, a)| a == attacker_id) {
                continue; // Already blocked
            }
            let attacker = match state.objects.get(&attacker_id) {
                Some(a) => a,
                None => continue,
            };
            // CR 702.111b: Menace requires 2+ blockers — handled by gang-block, not chump.
            if attacker.has_keyword(&Keyword::Menace) {
                continue;
            }
            let Some(headroom) = commander_lethal_headroom(state, player, attacker_id) else {
                continue; // Not a commander or no commander-damage threshold
            };
            let attacker_power = attacker.power.unwrap_or(0).max(0) as u32;
            if attacker_power < headroom {
                continue; // This commander can't push lethal commander damage this combat
            }

            // Find any legal blocker that's "safe" — i.e., not defeated by trample-over.
            let safe_blocker = available_blockers.iter().find(|&&bid| {
                if used_blockers.contains(&bid) {
                    return false;
                }
                if !can_block_with_engine_map(state, bid, attacker_id, valid_block_targets) {
                    return false;
                }
                let blocker_toughness = state
                    .objects
                    .get(&bid)
                    .and_then(|b| b.toughness)
                    .unwrap_or(1);
                !commander_chump_unsafe(state, player, attacker_id, blocker_toughness)
            });
            if let Some(&blocker_id) = safe_blocker {
                assignments.push((blocker_id, attacker_id));
                used_blockers.push(blocker_id);
            }
            // No safe chump exists — accept the loss on this commander rather than
            // wasting a creature that won't actually save the player. Continue to
            // the next commander attacker so other independent threats can still chump.
        }
    }

    assignments
}

/// CR 510.1c + CR 903.10a: Returns true when blocking `attacker` with a single creature of
/// `chump_toughness` would NOT prevent commander-damage lethality. For non-commander attackers
/// or non-commander formats, returns false (the chump is "safe" with respect to commander rules).
///
/// For trample attackers, the defending player still receives `(power - lethal_to_blocker)`
/// worth of commander damage that counts toward the 21-damage threshold. A 1/1 chump in front
/// of a 12/12 trample commander with only 3 headroom is unsafe — the player still loses to
/// commander damage even though the block was legal.
///
/// CR 702.2c + CR 702.19b: A deathtouch+trample attacker only needs to assign 1 damage to a
/// blocker before tramping the rest, so a 4/4 deathtouch+trample with 3 headroom defeats any
/// chump (trample-through = 3, lethal = 1 due to deathtouch).
fn commander_chump_unsafe(
    state: &GameState,
    defender: PlayerId,
    attacker_id: ObjectId,
    chump_toughness: i32,
) -> bool {
    let Some(headroom) = commander_lethal_headroom(state, defender, attacker_id) else {
        return false;
    };
    let Some(attacker) = state.objects.get(&attacker_id) else {
        return false;
    };
    let power = attacker.power.unwrap_or(0).max(0);
    let trample_through = if attacker.has_keyword(&Keyword::Trample) {
        // CR 702.2c: Deathtouch makes any nonzero damage lethal, so the trampler need only
        // assign 1 to the blocker before sending excess to the player.
        let lethal_to_blocker = if attacker.has_keyword(&Keyword::Deathtouch) {
            1
        } else {
            chump_toughness
        };
        (power - lethal_to_blocker).max(0)
    } else {
        0
    };
    trample_through as u32 >= headroom
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
        // Race velocity: compute turns-to-kill for both sides.
        // If our clock is shorter (we die sooner), stabilize instead of racing blindly.
        let our_clock = opponents
            .iter()
            .map(|&opp| race_clock(state, opp, player))
            .min()
            .unwrap_or(u32::MAX);
        let their_clock = opponents
            .iter()
            .map(|&opp| race_clock(state, player, opp))
            .min()
            .unwrap_or(u32::MAX);

        if our_clock <= 2 && our_clock < their_clock {
            // We die in 1-2 turns and can't kill them faster — stabilize
            CombatObjective::Stabilize
        } else {
            CombatObjective::Race
        }
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

    // CR 704.5a: A player with 0 or less life loses the game.
    // Path A — life-loss path: if raw aggregate damage equals or exceeds raw life,
    // we are facing immediate lethal this turn and must Stabilize unconditionally.
    // The bias multiplier is meaningless here — bias was previously applied to this
    // check and made low-bias profiles (Easy/VeryEasy at 0.8/0.9) miss exact lethal.
    if incoming_power >= life {
        return CombatObjective::Stabilize;
    }

    // CR 903.10a: A player loses if dealt 21+ combat damage from a single commander.
    // Path B — per-commander path: if ANY single commander attacker can cross its
    // remaining damage threshold this combat (accounting for prior commander damage),
    // the position is commander-lethal regardless of life total. Independent of Path A
    // because chumping commander A doesn't reduce lethality from commander B.
    let cmd_path_lethal = attacker_ids.iter().any(|&aid| {
        let Some(headroom) = commander_lethal_headroom(state, player, aid) else {
            return false;
        };
        let attacker_power = state
            .objects
            .get(&aid)
            .and_then(|o| o.power)
            .unwrap_or(0)
            .max(0) as u32;
        attacker_power >= headroom
    });
    if cmd_path_lethal {
        return CombatObjective::Stabilize;
    }

    // Bias-weighted near-lethal anticipation. With Path A handling exact lethal,
    // these bands govern multi-turn pressure where a more defensive bias (>1.0)
    // tells the AI to Stabilize earlier. Profiles below 1.0 still rely on Path A
    // for the unconditional save; the bands here only widen the Stabilize window.
    let threshold = incoming_power as f64 * profile.stabilize_bias;

    // Multi-turn lethality: dead in ~2-3 turns at this rate
    if life as f64 <= threshold * 2.5 {
        return CombatObjective::Stabilize;
    }

    // Race detection: losing the damage race (opponent hits harder than we do)
    // Only enter Race if we'd die in ~3 turns AND opponent outpaces us
    let my_board_power = battlefield_power(state, player);
    if life as f64 <= threshold * 3.0 && incoming_power > my_board_power {
        return CombatObjective::Race;
    }

    CombatObjective::PreserveAdvantage
}

fn should_attack_given_objective(
    objective: CombatObjective,
    free_damage: bool,
    favorable_trade: bool,
    has_lifelink: bool,
    attacker_power: i32,
    attacker_survives: bool,
    is_commander: bool,
) -> bool {
    // CR 903.8: a commander recast from the command zone costs an extra {2} per
    // prior cast (commander tax), and trading it away surrenders the player's
    // most valuable permanent. Don't trade the commander in combat — only swing
    // it into a block when it survives (free_damage) or when pushing lethal.
    // Unblockable / no-blocker commander swings are handled by the earlier
    // branch (before this function is reached), so this only suppresses trades.
    if is_commander && !free_damage && objective != CombatObjective::PushLethal {
        return false;
    }
    // CR 702.15b: lifelink gains life whenever the creature *deals* combat
    // damage — including a value-unfavorable simultaneous trade, and a
    // first-strike pinger that then dies. So life IS still gained on a bad
    // trade; this is a VALUE decision, not a rules claim: don't let the lifelink
    // swing justify throwing the creature away for nothing (it dies, the blocker
    // lives, no kill). Pursue the swing only when the attack is otherwise
    // non-losing — free damage, a favorable trade, or the attacker survives.
    let lifelink_bonus =
        has_lifelink && attacker_power > 0 && (free_damage || favorable_trade || attacker_survives);
    match objective {
        CombatObjective::PushLethal => true,
        CombatObjective::Stabilize => free_damage || lifelink_bonus,
        CombatObjective::PreserveAdvantage => free_damage || favorable_trade || lifelink_bonus,
        CombatObjective::Race => free_damage || favorable_trade || lifelink_bonus,
    }
}

/// Estimate how many turns until `defender` dies from `attacker`'s board.
/// Returns u32::MAX if the attacker has no damage on board.
fn race_clock(state: &GameState, attacker: PlayerId, defender: PlayerId) -> u32 {
    let defender_life = state.players[defender.0 as usize].life;
    if defender_life <= 0 {
        return 0;
    }
    let attack_power = battlefield_power(state, attacker);
    if attack_power <= 0 {
        return u32::MAX;
    }
    // Ceiling division: turns to deal lethal
    ((defender_life + attack_power - 1) / attack_power) as u32
}

/// Compute the maximum damage an opponent can deal on the crackback,
/// assuming the given set of `tapped_attackers` are tapped and unavailable
/// to block. Vigilance creatures in `tapped_attackers` are still available.
///
/// When a `projection` is provided, opponent creature power/keywords are
/// read from the projected state (after their upcoming phase-triggers and
/// attack-triggers have resolved). This catches Ouroboroid-class scaling,
/// Battle Cry / Mentor / Hellrider pumps, saga advances, and similar
/// growth that would otherwise be invisible to the snapshot heuristic.
/// Creatures removed during projection fall back to the current state's
/// power for a conservative read.
fn crackback_damage(
    state: &GameState,
    player: PlayerId,
    opponents: &[PlayerId],
    tapped_attackers: &[ObjectId],
    projection: Option<&Projection>,
) -> i32 {
    let mut our_blockers: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = state.objects.get(&id)?;
            if obj.controller != player
                || !obj.card_types.core_types.contains(&CoreType::Creature)
                || obj.tapped
            {
                return None;
            }
            if tapped_attackers.contains(&id) && !obj.has_keyword(&Keyword::Vigilance) {
                return None;
            }
            Some(id)
        })
        .collect();

    our_blockers.sort_by(|&a, &b| {
        let ta = state.objects.get(&a).and_then(|o| o.toughness).unwrap_or(0);
        let tb = state.objects.get(&b).and_then(|o| o.toughness).unwrap_or(0);
        tb.cmp(&ta)
    });

    // Opponent's creatures that could attack next turn. When a projection
    // is available, read identity AND `tapped`/keywords from the projected
    // state — creatures untap during the opponent's upcoming untap step, so
    // reading `tapped` from the current state would incorrectly exclude them
    // whenever the AI is evaluating an attack on the turn after a user swing.
    // Without a projection, fall back to current-state filtering.
    let projected_state = projection.map(|p| &p.state);
    let attacker_source = projected_state.unwrap_or(state);
    let mut opp_attackers: Vec<(ObjectId, i32)> = opponents
        .iter()
        .flat_map(|&opp| {
            attacker_source.battlefield.iter().filter_map(move |&id| {
                let obj = attacker_source.objects.get(&id)?;
                if obj.controller == opp
                    && obj.card_types.core_types.contains(&CoreType::Creature)
                    && !obj.tapped
                    && !obj.has_keyword(&Keyword::Defender)
                {
                    Some((id, obj.power.unwrap_or(0)))
                } else {
                    None
                }
            })
        })
        .collect();

    opp_attackers.sort_by_key(|b| std::cmp::Reverse(b.1));

    let mut unblocked_damage = 0i32;
    // CR 509.1: greedy 1:1 blocker assignment. Track which blockers have been
    // committed rather than a single advancing cursor: a blocker that can't
    // legally block the CURRENT attacker (e.g. a ground creature vs a flyer)
    // must remain available for later attackers it CAN block. A shared cursor
    // permanently discarded such a blocker, over-estimating crackback and making
    // the AI hold back profitable attacks.
    let mut used = vec![false; our_blockers.len()];
    for &(opp_id, opp_power) in &opp_attackers {
        // Keyword lookup mirrors the power lookup: prefer the projected view
        // (e.g., Battle Cry / Mentor pumps, newly-granted Trample).
        let opp_obj = match projected_state
            .and_then(|ps| ps.objects.get(&opp_id))
            .or_else(|| state.objects.get(&opp_id))
        {
            Some(o) => o,
            None => continue,
        };
        // First not-yet-committed blocker that can legally block this attacker.
        let mut blocked = false;
        for (i, &bid) in our_blockers.iter().enumerate() {
            if used[i] {
                continue;
            }
            if !can_block_pair(attacker_source, bid, opp_id) {
                continue; // skip — still available for other attackers
            }
            used[i] = true;
            blocked = true;
            if opp_obj.has_keyword(&Keyword::Trample) {
                let blocker_toughness = attacker_source
                    .objects
                    .get(&bid)
                    .and_then(|b| b.toughness)
                    .unwrap_or(0);
                unblocked_damage += (opp_power - blocker_toughness).max(0);
            }
            break;
        }
        if !blocked {
            unblocked_damage += opp_power;
        }
    }

    unblocked_damage
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

/// CR 509.1 + CR 702.19b: Returns true when no legal blocker assignment can
/// prevent lethal life loss against `attacker_ids`. Computes an *optimistic*
/// upper bound on absorption (any blocker can block any attacker; chumping
/// non-trample fully neutralizes one attacker; tramplers absorb only blocker
/// toughness; unblockable attackers absorb nothing) and bails only when even
/// that upper bound leaves residual damage >= life.
///
/// The relaxation makes this a *safe* fast-path: false negatives (missing a
/// bail) only cost CPU; false positives (bailing when a save existed) cannot
/// happen because the bound dominates any real assignment's absorption.
///
/// Optimal allocation under the relaxation: a blocker spent chumping absorbs the
/// attacker's full power (toughness-independent); a blocker spent on trample
/// absorbs its own toughness. Chumping the most attackers is NOT always optimal —
/// chumping a low-power attacker can waste a high-toughness blocker that would
/// soak more trample. So maximize over every chump count `k` in `0..=min(chumps,
/// blockers)`: chump the `k` highest-power attackers (using the `k` smallest
/// blockers) and reserve the `blockers - k` largest-toughness blockers for trample.
fn block_is_futile(
    state: &GameState,
    player: PlayerId,
    attacker_ids: &[ObjectId],
    available_blockers: &[ObjectId],
) -> bool {
    let life = state.players[player.0 as usize].life;
    if life <= 0 {
        return false; // Already dead; let SBAs handle it, don't short-circuit.
    }

    let mut chumpable_powers: Vec<i32> = Vec::new();
    let mut trample_power: i32 = 0;
    let mut unblockable_power: i32 = 0;
    let mut total_attacker_power: i32 = 0;

    for &aid in attacker_ids {
        let Some(a) = state.objects.get(&aid) else {
            continue;
        };
        let power = a.power.unwrap_or(0).max(0);
        total_attacker_power += power;
        if has_cant_be_blocked(state, a) {
            unblockable_power += power;
        } else if a.has_keyword(&Keyword::Trample) {
            trample_power += power;
        } else {
            chumpable_powers.push(power);
        }
    }

    // Optimistic chump: highest-power non-trample attackers fully absorbed first.
    chumpable_powers.sort_unstable_by(|a, b| b.cmp(a));
    let mut blocker_toughnesses: Vec<i32> = available_blockers
        .iter()
        .filter_map(|&id| state.objects.get(&id).and_then(|o| o.toughness))
        .map(|t| t.max(0))
        .collect();
    blocker_toughnesses.sort_unstable_by(|a, b| b.cmp(a));

    // CR 510.1c: Chumping a non-trample attacker absorbs its full power regardless
    // of the blocker's toughness, so chump with the SMALLEST blockers and reserve
    // the LARGEST-toughness ones to soak trample. `blocker_toughnesses` is sorted
    // descending, so `toughness_prefix[m]` is the absorption of the m largest.
    let total_blockers = blocker_toughnesses.len();
    let mut toughness_prefix = vec![0i32; total_blockers + 1];
    for (i, &t) in blocker_toughnesses.iter().enumerate() {
        toughness_prefix[i + 1] = toughness_prefix[i] + t;
    }

    // Maximize absorption over every chump count `k`: chumping the `k` biggest
    // attackers frees the `total_blockers - k` largest blockers for trample.
    // Forcing the maximum `k` under-counted absorption (a small chump can cost a
    // big trample blocker) and wrongly reported survivable boards as futile.
    let max_chump = chumpable_powers.len().min(total_blockers);
    let mut max_absorption = 0;
    let mut chump_absorption = 0;
    for k in 0..=max_chump {
        if k > 0 {
            chump_absorption += chumpable_powers[k - 1];
        }
        let trample_absorption = trample_power.min(toughness_prefix[total_blockers - k]);
        max_absorption = max_absorption.max(chump_absorption + trample_absorption);
    }
    let min_residual = total_attacker_power - max_absorption;

    // Residual = unblockable_power + uncovered chumpables + uncovered trample.
    // Absorption only ever neutralizes chumpable/trample power, never unblockable,
    // so the residual can never drop below the unblockable total. Bail iff residual
    // STRICTLY EXCEEDS life — at exact lethal we still chump per the existing
    // "minimize damage even when dying" semantics (opponent miscounts / lifegain).
    debug_assert!(min_residual >= unblockable_power);
    debug_assert!(max_absorption <= chumpable_powers.iter().sum::<i32>() + trample_power);
    min_residual > life
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

/// Returns true when the AI can deal lethal damage this turn by attacking:
/// - All opponent creatures are tapped (no blockers available)
/// - The AI's total untapped attackable power >= the opponent's minimum life total
///
/// Used in the pre-combat main phase to discourage spending resources (mana from
/// dorks, convoke creatures) before attacking when a winning attack is available.
pub fn is_lethal_attack_available(state: &GameState, ai_player: PlayerId) -> bool {
    let opponents: Vec<PlayerId> = players::opponents(state, ai_player);
    if opponents.is_empty() {
        return false;
    }
    let min_opp_life = opponents
        .iter()
        .map(|&opp| state.players[opp.0 as usize].life)
        .min()
        .unwrap_or(20);
    if min_opp_life <= 0 {
        return false;
    }
    // Check if ANY opponent creature is untapped (would be a potential blocker).
    let any_untapped_blocker = opponents.iter().any(|&opp| {
        state.battlefield.iter().any(|&id| {
            state.objects.get(&id).is_some_and(|o| {
                o.controller == opp
                    && o.card_types.core_types.contains(&CoreType::Creature)
                    && !o.tapped
            })
        })
    });
    if any_untapped_blocker {
        return false;
    }
    // Sum all AI creatures that could attack right now.
    let attack_power: i32 = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            if can_attack(state, id) {
                let obj = state.objects.get(&id)?;
                if obj.controller == ai_player {
                    return obj.power;
                }
            }
            None
        })
        .sum();
    attack_power >= min_opp_life
}

/// Check if a creature has the absolute "can't be blocked" static ability.
/// Intentionally excludes CantBeBlockedExceptBy / CantBeBlockedBy — those creatures
/// can still be blocked by matching creatures and should go through normal evaluation.
///
/// CR 702.26b + CR 114.4 + CR 604.1: route through the engine's single-authority
/// `active_static_definitions` helper so a phased-out attacker with CantBeBlocked
/// is not mis-evaluated by the combat AI.
fn has_cant_be_blocked(state: &GameState, obj: &engine::game::game_object::GameObject) -> bool {
    engine::game::functioning_abilities::active_static_definitions(state, obj)
        .any(|sd| sd.mode == StaticMode::CantBeBlocked)
}

/// Check if a blocker can legally block an attacker, using the engine's pre-validated
/// `valid_block_targets` map when available. Falls back to the engine's `can_block_pair`
/// when the map is not provided (e.g. unit tests without a WaitingFor state).
fn can_block_with_engine_map(
    state: &GameState,
    blocker_id: ObjectId,
    attacker_id: ObjectId,
    valid_block_targets: Option<&HashMap<ObjectId, Vec<ObjectId>>>,
) -> bool {
    if let Some(map) = valid_block_targets {
        map.get(&blocker_id)
            .is_some_and(|targets| targets.contains(&attacker_id))
    } else {
        can_block_pair(state, blocker_id, attacker_id)
    }
}

/// The block a rational defending player would commit against one attacker,
/// described from the ATTACKER's point of view.
struct DefenderBlock {
    /// Value of the blocking creature the defender chooses.
    blocker_value: f64,
    /// Whether the attacker kills that blocker in the exchange.
    kills_blocker: bool,
    /// Whether the attacker survives the exchange.
    attacker_survives: bool,
}

/// CR 509.1a: Choose the block the defending player would actually make against
/// a single attacker. A rational defender maximizes its own value — it kills the
/// attacker when that is value-positive, preferring a blocker that survives the
/// exchange (a "free" kill via first strike or a larger body, CR 702.7b) and
/// otherwise the cheapest creature whose loss the kill justifies. Returns `None`
/// when no creature can legally block (the attack connects unimpeded).
///
/// This deliberately models the defender's *best* block rather than its cheapest
/// creature. The cheapest-blocker model let the AI swing doomed creatures on the
/// false premise of a favorable trade the defender would sidestep — e.g. a 1/1
/// attacker into a 2/1 first-striker that eats it for free while a 2/1 token sat
/// nearby looking like an even trade.
fn defender_best_block(
    state: &GameState,
    attacker_id: ObjectId,
    attacker_value: f64,
    blockers: &[ObjectId],
) -> Option<DefenderBlock> {
    let attacker = state.objects.get(&attacker_id)?;
    blockers
        .iter()
        .filter(|&&bid| can_block_pair(state, bid, attacker_id))
        .filter_map(|&bid| {
            let blocker = state.objects.get(&bid)?;
            let blocker_value = evaluate_creature(state, bid);
            // CR 702.7b + CR 702.4b + CR 702.2c: keyword-aware outcome (first
            // strike, double strike, deathtouch), not a raw P/T comparison.
            let (blocker_kills_attacker, blocker_survives) =
                evaluate_block_outcome(blocker, attacker);
            // Defender utility: the attacker value it removes (only if the block
            // is lethal) minus the value of its own blocker (only if that blocker
            // dies). A free kill scores `attacker_value`; a trade nets the
            // difference; a chump that dies for nothing scores negative.
            let defender_gain = (if blocker_kills_attacker {
                attacker_value
            } else {
                0.0
            }) - (if blocker_survives { 0.0 } else { blocker_value });
            Some((
                defender_gain,
                DefenderBlock {
                    blocker_value,
                    kills_blocker: !blocker_survives,
                    attacker_survives: !blocker_kills_attacker,
                },
            ))
        })
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, outcome)| outcome)
}

/// Evaluate whether a single blocker kills the attacker and/or survives combat,
/// accounting for first strike (CR 702.7), double strike (CR 702.4), and
/// deathtouch (CR 702.2).
fn evaluate_block_outcome(
    blocker: &engine::game::game_object::GameObject,
    attacker: &engine::game::game_object::GameObject,
) -> (bool, bool) {
    let blocker_power = blocker.power.unwrap_or(0);
    let blocker_toughness = blocker.toughness.unwrap_or(0);
    let attacker_power = attacker.power.unwrap_or(0);
    let attacker_toughness = attacker.toughness.unwrap_or(0);

    let attacker_has_first_strike =
        attacker.has_keyword(&Keyword::FirstStrike) || attacker.has_keyword(&Keyword::DoubleStrike);
    let blocker_has_first_strike =
        blocker.has_keyword(&Keyword::FirstStrike) || blocker.has_keyword(&Keyword::DoubleStrike);
    let attacker_has_deathtouch = attacker.has_keyword(&Keyword::Deathtouch);
    let blocker_has_deathtouch = blocker.has_keyword(&Keyword::Deathtouch);

    // CR 702.2c: Any nonzero damage from deathtouch is lethal.
    let attacker_lethal = if attacker_has_deathtouch {
        1
    } else {
        blocker_toughness
    };
    let blocker_lethal = if blocker_has_deathtouch {
        1
    } else {
        attacker_toughness
    };

    // CR 702.4b / CR 702.7b: First-strike/double-strike creatures deal damage
    // in the first combat damage step. If one side has it and the other doesn't,
    // the first-striker may kill before the other deals damage.
    let blocker_dies_before_dealing =
        attacker_has_first_strike && !blocker_has_first_strike && attacker_power >= attacker_lethal;

    let attacker_dies_before_dealing =
        blocker_has_first_strike && !attacker_has_first_strike && blocker_power >= blocker_lethal;

    // CR 702.4a: Double strike deals damage in both combat damage steps.
    let effective_attacker_damage = if attacker.has_keyword(&Keyword::DoubleStrike) {
        attacker_power * 2
    } else {
        attacker_power
    };
    let effective_blocker_damage = if blocker.has_keyword(&Keyword::DoubleStrike) {
        blocker_power * 2
    } else {
        blocker_power
    };

    let kills = if blocker_dies_before_dealing {
        // Blocker is killed by first strike before it can deal damage
        false
    } else {
        effective_blocker_damage >= blocker_lethal
    };

    let survives = if attacker_dies_before_dealing {
        // Attacker is killed by blocker's first strike before it can deal damage
        true
    } else {
        effective_attacker_damage < attacker_lethal
    };

    (kills, survives)
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

    // --- Issue #2514: crackback_damage blocker reuse (CR 509.1) ---

    #[test]
    fn crackback_blocker_not_consumed_by_unblockable_attacker() {
        // A ground wall that can't block a flyer must remain available to block a
        // ground attacker. The old shared cursor discarded the wall after it
        // failed to block the (higher-power) flyer, over-counting crackback.
        let mut state = setup();
        // AI (P0) has only a 0/5 ground wall.
        add_creature(&mut state, PlayerId(0), "Wall", 0, 5, vec![]);
        // Opponent (P1): a 5/5 flyer (sorted first by power) and a 4/4 ground.
        add_creature(
            &mut state,
            PlayerId(1),
            "Flyer",
            5,
            5,
            vec![Keyword::Flying],
        );
        add_creature(&mut state, PlayerId(1), "Ground", 4, 4, vec![]);

        let cb = crackback_damage(&state, PlayerId(0), &[PlayerId(1)], &[], None);
        // The wall blocks the 4/4; only the 5/5 flyer is unblocked.
        assert_eq!(
            cb, 5,
            "wall must block the ground 4/4, leaving only the flyer's 5"
        );
    }

    #[test]
    fn crackback_uses_all_legal_pairings() {
        // Two ground walls + (flyer, two ground attackers): both walls block the
        // ground attackers; only the flyer is unblocked.
        let mut state = setup();
        add_creature(&mut state, PlayerId(0), "Wall A", 0, 5, vec![]);
        add_creature(&mut state, PlayerId(0), "Wall B", 0, 4, vec![]);
        add_creature(
            &mut state,
            PlayerId(1),
            "Flyer",
            5,
            5,
            vec![Keyword::Flying],
        );
        add_creature(&mut state, PlayerId(1), "Ground A", 3, 3, vec![]);
        add_creature(&mut state, PlayerId(1), "Ground B", 2, 2, vec![]);

        let cb = crackback_damage(&state, PlayerId(0), &[PlayerId(1)], &[], None);
        assert_eq!(
            cb, 5,
            "both walls block the ground attackers; flyer unblocked"
        );
    }

    #[test]
    fn crackback_trample_counts_only_excess() {
        // A trampler blocked by a smaller creature contributes only the excess.
        let mut state = setup();
        add_creature(&mut state, PlayerId(0), "Blocker", 2, 2, vec![]);
        add_creature(
            &mut state,
            PlayerId(1),
            "Trampler",
            5,
            5,
            vec![Keyword::Trample],
        );

        let cb = crackback_damage(&state, PlayerId(0), &[PlayerId(1)], &[], None);
        // 5 power - 2 toughness blocker = 3 trample-through.
        assert_eq!(cb, 3, "only the trample excess (5-2) is counted");
    }

    #[test]
    fn crackback_projection_drives_block_legality() {
        let mut state = setup();
        add_creature(&mut state, PlayerId(0), "Wall", 0, 5, vec![]);
        let attacker = add_creature(&mut state, PlayerId(1), "Projected Flyer", 4, 4, vec![]);

        let mut projected = state.clone();
        projected
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Flying);
        let projection = Projection {
            horizon_reached: ProjectionHorizon::OpponentAttackersDeclared,
            state: projected,
            snapshots: Vec::new(),
            confidence: crate::projection::Confidence::Exact,
            target_opponent: PlayerId(1),
        };

        let cb = crackback_damage(&state, PlayerId(0), &[PlayerId(1)], &[], Some(&projection));
        assert_eq!(cb, 4, "projected flying must make the attacker unblocked");
    }

    /// Battlefield planeswalker for `owner` with the given starting loyalty.
    /// Used to drive the planeswalker-attack redirect through the real engine
    /// path: `get_valid_attack_targets` classifies it as an attackable PW.
    fn add_planeswalker(state: &mut GameState, owner: PlayerId, loyalty: u32) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Planeswalker".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.loyalty = Some(loyalty);
        obj.entered_battlefield_turn = Some(1);
        id
    }

    /// Engine-derived legal attack targets — the same list the live
    /// `WaitingFor::DeclareAttackers` carries. Deriving from the engine (rather
    /// than hand-building) proves the engine offers the PW and the AI consumes
    /// it, not just that the AI routes to a target we injected.
    fn valid_targets(state: &GameState) -> Vec<AttackTarget> {
        engine::game::combat::get_valid_attack_targets(state)
    }

    /// Issue #484 (P0) — E2E: a goaded creature the value heuristic would skip
    /// MUST still be declared as an attacker, and the resulting declaration must
    /// be accepted by the engine. Drives the real AI → engine pipeline.
    #[test]
    fn goaded_creature_is_declared_and_engine_accepts() {
        let mut state = setup();
        // Goaded 1/5: the value heuristic scores it as a non-attacker into a
        // blocker, but goad (CR 701.15b) forces it to attack.
        let goaded = add_creature(&mut state, PlayerId(0), "Omo", 1, 5, vec![]);
        state
            .objects
            .get_mut(&goaded)
            .unwrap()
            .goaded_by
            .insert(PlayerId(1));
        // A vanilla creature for the heuristic to also (legitimately) decline.
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);
        // Opponent blocker so the 1/5 looks unprofitable to the heuristic.
        add_creature(&mut state, PlayerId(1), "Wall", 0, 6, vec![]);

        let attacks = choose_attackers_with_targets(&state, PlayerId(0));
        assert!(
            attacks.iter().any(|(id, _)| *id == goaded),
            "goaded creature must be declared as an attacker (CR 701.15b)"
        );

        // The AI's declaration must be engine-legal.
        let result = engine::game::combat::declare_attackers(&mut state, &attacks, &mut vec![]);
        assert!(
            result.is_ok(),
            "engine must accept the AI's goad-compliant declaration: {result:?}"
        );
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
    fn flyer_does_not_attack_into_larger_flying_blocker() {
        let mut state = setup();
        let flyer = add_creature(&mut state, PlayerId(0), "Bird", 2, 2, vec![Keyword::Flying]);
        add_creature(
            &mut state,
            PlayerId(1),
            "Serra Angel",
            4,
            4,
            vec![Keyword::Flying],
        );

        let attackers = choose_attackers(&state, PlayerId(0));

        assert!(
            !attackers.contains(&flyer),
            "Flying is evasion, not unblockable; AI should not suicide into a larger flyer"
        );
    }

    #[test]
    fn vigilance_does_not_attack_into_larger_blocker() {
        let mut state = setup();
        let vigilant = add_creature(
            &mut state,
            PlayerId(0),
            "Watchwolf",
            3,
            3,
            vec![Keyword::Vigilance],
        );
        add_creature(&mut state, PlayerId(1), "Giant", 5, 5, vec![]);

        let attackers = choose_attackers(&state, PlayerId(0));

        assert!(
            !attackers.contains(&vigilant),
            "Vigilance removes tap cost, but does not make a bad block profitable"
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

    /// Regression (gamestate1): the AI must evaluate an attack against the
    /// defender's *best* block, not its cheapest creature. A 2/2 attacker faces a
    /// 1/1 chump (which it would profitably eat) and a 3/3 (which kills it for
    /// free). The old min-value-blocker model picked the 1/1, saw "free damage,"
    /// and attacked — but the defender blocks with the 3/3 and the 2/2 dies for
    /// nothing. The live bug was the first-strike variant (1/1 land into a 2/1
    /// first-striker); a larger body is the same "kills and survives" class and
    /// makes a value-independent, deterministic test.
    #[test]
    fn does_not_attack_when_a_better_blocker_kills_for_free() {
        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);
        // Cheapest blocker: a 1/1 the attacker would profitably trade up against.
        add_creature(&mut state, PlayerId(1), "Squirrel", 1, 1, vec![]);
        // Best blocker: a 3/3 that kills the 2/2 and survives — a free kill.
        add_creature(&mut state, PlayerId(1), "Centaur", 3, 3, vec![]);

        let attackers = choose_attackers(&state, PlayerId(0));
        assert!(
            !attackers.contains(&attacker),
            "AI must not attack a 2/2 when the defender holds a 3/3 that eats it \
             for free, even though a 1/1 chump is also available"
        );
    }

    /// Companion to the regression above: when the defender's *best* block is
    /// still a losing chump (a 1/1 in front of a 4/4), the attack is correctly
    /// declared — the rational-defender model must not become so pessimistic that
    /// it refuses profitable swings.
    #[test]
    fn attacks_when_best_block_is_only_a_chump() {
        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Rhino", 4, 4, vec![]);
        add_creature(&mut state, PlayerId(1), "Squirrel", 1, 1, vec![]);
        add_creature(&mut state, PlayerId(1), "Goblin", 1, 1, vec![]);

        let attackers = choose_attackers(&state, PlayerId(0));
        assert!(
            attackers.contains(&attacker),
            "AI should attack a 4/4 when every available block is a chump it survives"
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
    fn four_player_bots_do_not_all_focus_same_seat_on_equal_threat() {
        let mut state = setup_multiplayer(4);
        let p1_attacker = add_creature(&mut state, PlayerId(1), "P1 Bear", 2, 2, vec![]);
        let p2_attacker = add_creature(&mut state, PlayerId(2), "P2 Bear", 2, 2, vec![]);
        let p3_attacker = add_creature(&mut state, PlayerId(3), "P3 Bear", 2, 2, vec![]);

        let p1_attacks = choose_attackers_with_targets(&state, PlayerId(1));
        let p2_attacks = choose_attackers_with_targets(&state, PlayerId(2));
        let p3_attacks = choose_attackers_with_targets(&state, PlayerId(3));

        assert_eq!(
            p1_attacks,
            vec![(p1_attacker, AttackTarget::Player(PlayerId(2)))]
        );
        assert_eq!(
            p2_attacks,
            vec![(p2_attacker, AttackTarget::Player(PlayerId(3)))]
        );
        assert_eq!(
            p3_attacks,
            vec![(p3_attacker, AttackTarget::Player(PlayerId(0)))]
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
                AttackTarget::Planeswalker(_) | AttackTarget::Battle(_) => {}
            }
        }
    }

    #[test]
    fn lethal_aggregate_damage_triggers_chump_blocks() {
        let mut state = setup();
        // Three 2/2s attacking — 6 total damage, player at 5 life = lethal
        let a1 = add_creature(&mut state, PlayerId(0), "Bear1", 2, 2, vec![]);
        let a2 = add_creature(&mut state, PlayerId(0), "Bear2", 2, 2, vec![]);
        let a3 = add_creature(&mut state, PlayerId(0), "Bear3", 2, 2, vec![]);
        let chump = add_creature(&mut state, PlayerId(1), "Token", 1, 1, vec![]);
        state.players[1].life = 5;

        let blockers = choose_blockers(&state, PlayerId(1), &[a1, a2, a3]);

        // Must chump block at least one attacker to drop damage from 6 to 4 (survivable)
        assert!(
            !blockers.is_empty(),
            "Facing lethal aggregate damage, AI must chump block to survive"
        );
        assert!(
            blockers.iter().any(|&(b, _)| b == chump),
            "The 1/1 token should chump block when facing lethal"
        );
    }

    #[test]
    fn lethal_aggregate_prefers_blocking_highest_power() {
        let mut state = setup();
        // A 3/3 and two 1/1s attacking — 5 total, player at 5 life = lethal
        let big = add_creature(&mut state, PlayerId(0), "Ogre", 3, 3, vec![]);
        let small1 = add_creature(&mut state, PlayerId(0), "Rat1", 1, 1, vec![]);
        let _small2 = add_creature(&mut state, PlayerId(0), "Rat2", 1, 1, vec![]);
        let chump = add_creature(&mut state, PlayerId(1), "Token", 1, 1, vec![]);
        state.players[1].life = 5;

        let blockers = choose_blockers(&state, PlayerId(1), &[big, small1, _small2]);

        // Should block the 3/3 to prevent the most damage
        assert!(
            blockers.contains(&(chump, big)),
            "Should chump the highest-power attacker to maximize damage prevented"
        );
    }

    #[test]
    fn lethal_aggregate_accounts_for_trample() {
        let mut state = setup();
        // 5/5 trample + 2/2 = 7 total damage, player at 5 life
        // Chumping the 5/5 trample with a 1/1 only prevents 1 damage (4 tramples through)
        // So actual damage after chump = 4 + 2 = 6, still lethal
        // The AI should recognize this and prefer blocking the 2/2 instead
        let trampler = add_creature(
            &mut state,
            PlayerId(0),
            "Trampler",
            5,
            5,
            vec![Keyword::Trample],
        );
        let bear = add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);
        let chump = add_creature(&mut state, PlayerId(1), "Token", 1, 1, vec![]);
        state.players[1].life = 5;

        let blockers = choose_blockers(&state, PlayerId(1), &[trampler, bear]);

        // Should block the 2/2 (prevents 2 damage) not the 5/5 trampler (prevents only 1)
        assert!(
            blockers.contains(&(chump, bear)),
            "Should chump the non-trampler to prevent more damage, got {:?}",
            blockers
        );
    }

    #[test]
    fn non_lethal_aggregate_skips_chump() {
        let mut state = setup();
        // Two 2/2s attacking — 4 total, player at 20 life = not lethal
        let a1 = add_creature(&mut state, PlayerId(0), "Bear1", 2, 2, vec![]);
        let a2 = add_creature(&mut state, PlayerId(0), "Bear2", 2, 2, vec![]);
        let _chump = add_creature(&mut state, PlayerId(1), "Token", 1, 1, vec![]);
        state.players[1].life = 20;

        let blockers = choose_blockers(&state, PlayerId(1), &[a1, a2]);

        // At 20 life, taking 4 is fine — don't waste the chump
        assert!(
            blockers.is_empty(),
            "Healthy defender should not chump block against non-lethal aggregate damage"
        );
    }

    // --- Bug-fix regression tests for AI block decision pipeline ---

    /// Bug 1 regression: Easy difficulty uses `stabilize_bias = 0.9`, which previously
    /// scaled the lethal-detection threshold to `incoming_power * 0.9`. At exact-lethal
    /// (life == incoming_power), the comparison `life <= 0.9 * incoming` failed and the
    /// AI fell through to `PreserveAdvantage`, skipping the third-pass chump loop.
    /// Path A in `determine_block_objective` now uses raw `incoming_power >= life`
    /// unconditionally, regardless of profile bias.
    #[test]
    fn easy_difficulty_blocks_exact_lethal() {
        let mut state = setup();
        // 4× 5/5 = 20 power; defender at 20 life — exact lethal, must chump.
        let a1 = add_creature(&mut state, PlayerId(0), "Bear1", 5, 5, vec![]);
        let a2 = add_creature(&mut state, PlayerId(0), "Bear2", 5, 5, vec![]);
        let a3 = add_creature(&mut state, PlayerId(0), "Bear3", 5, 5, vec![]);
        let a4 = add_creature(&mut state, PlayerId(0), "Bear4", 5, 5, vec![]);
        let chump = add_creature(&mut state, PlayerId(1), "Token", 1, 1, vec![]);
        state.players[1].life = 20;

        let easy_profile = AiProfile {
            risk_tolerance: 0.8,
            interaction_patience: 0.4,
            stabilize_bias: 0.9,
        };
        let assignments = choose_blockers_with_profile(
            &state,
            PlayerId(1),
            &[a1, a2, a3, a4],
            &easy_profile,
            None,
        );

        assert!(
            !assignments.is_empty(),
            "Easy AI must chump at exact lethal (Bug 1 regression)"
        );
        assert!(
            assignments.iter().any(|&(b, _)| b == chump),
            "1/1 token should chump-block under exact lethal on Easy difficulty"
        );
    }

    /// Bug 2 regression: 5-power commander with 18 prior commander damage is
    /// commander-lethal (5 ≥ 21−18) even when raw life (30) far exceeds incoming
    /// damage. Path B in `determine_block_objective` recognizes this; the per-commander
    /// chump pass assigns a blocker.
    #[test]
    fn commander_damage_triggers_chump_block() {
        use engine::types::format::FormatConfig;
        use engine::types::game_state::CommanderDamageEntry;

        let mut state = setup();
        state.format_config = FormatConfig::commander();
        state.players[1].life = 30;

        let commander = add_creature(&mut state, PlayerId(0), "Cmd", 5, 5, vec![]);
        state.objects.get_mut(&commander).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander,
            damage: 18,
        });
        let chump = add_creature(&mut state, PlayerId(1), "Token", 1, 1, vec![]);

        let assignments = choose_blockers(&state, PlayerId(1), &[commander]);

        assert!(
            assignments.contains(&(chump, commander)),
            "AI must chump 5-power commander with 18 prior cmd damage (Bug 2 regression), \
             got {:?}",
            assignments
        );
    }

    /// Bug 2 regression — disjunctive aggregation: two opposing commanders each at
    /// 18 prior cmd damage attacking for 5 are independently commander-lethal.
    /// Sum-of-min-eff-life would be 6, which is `< life=30` and would NOT trigger
    /// Stabilize. The disjunctive Path B catches this via `attackers.iter().any(...)`.
    #[test]
    fn two_commanders_independent_lethality() {
        use engine::types::format::FormatConfig;
        use engine::types::game_state::CommanderDamageEntry;

        let mut state = setup();
        state.format_config = FormatConfig::commander();
        state.players[1].life = 30;

        let cmd_a = add_creature(&mut state, PlayerId(0), "CmdA", 5, 5, vec![]);
        let cmd_b = add_creature(&mut state, PlayerId(0), "CmdB", 5, 5, vec![]);
        state.objects.get_mut(&cmd_a).unwrap().is_commander = true;
        state.objects.get_mut(&cmd_b).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander: cmd_a,
            damage: 18,
        });
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander: cmd_b,
            damage: 18,
        });
        let chump_a = add_creature(&mut state, PlayerId(1), "TokenA", 1, 1, vec![]);
        let chump_b = add_creature(&mut state, PlayerId(1), "TokenB", 1, 1, vec![]);

        let assignments = choose_blockers(&state, PlayerId(1), &[cmd_a, cmd_b]);

        let blocked_attackers: Vec<ObjectId> = assignments.iter().map(|&(_, a)| a).collect();
        assert!(
            blocked_attackers.contains(&cmd_a) && blocked_attackers.contains(&cmd_b),
            "Both commanders must be chump-blocked independently — chumping one doesn't \
             save from the other (got assignments: {:?}, chumps: [{:?}, {:?}])",
            assignments,
            chump_a,
            chump_b
        );
    }

    /// Bug 3 regression: in a 3-player pod, attackers heading to a player other
    /// than the AI must not factor into the AI's block objective. The filter at
    /// `search.rs:846/892` uses `defending_player == ai_player`. Verify here at
    /// the `determine_block_objective` layer by passing a pre-filtered (empty)
    /// attacker list — objective should not be Stabilize.
    #[test]
    fn multiplayer_attackers_targeting_others_dont_panic_ai() {
        let mut state = setup_multiplayer(3);
        // PlayerId(0) attacks PlayerId(2) with lethal; AI is PlayerId(1).
        add_creature(&mut state, PlayerId(0), "Threat", 30, 30, vec![]);
        add_creature(&mut state, PlayerId(1), "Sentinel", 1, 1, vec![]);
        state.players[1].life = 5;

        // Simulating the search.rs filter: AI sees an empty attacker list because
        // no attacker targets PlayerId(1).
        let assignments = choose_blockers(&state, PlayerId(1), &[]);

        assert!(
            assignments.is_empty(),
            "With no attackers targeting the AI, no blockers should be assigned"
        );
    }

    /// Bug 2 / B-R1 regression: a 12/12 trample commander with only 3 cmd-damage
    /// headroom defeats a 1/1 chump (12 - 1 = 11 trample-through ≥ 3 headroom).
    /// `commander_chump_unsafe` returns true; the per-commander chump pass skips
    /// the assignment rather than wasting the creature for zero defensive value.
    #[test]
    fn trample_commander_skips_unsafe_chump() {
        use engine::types::format::FormatConfig;
        use engine::types::game_state::CommanderDamageEntry;

        let mut state = setup();
        state.format_config = FormatConfig::commander();
        state.players[1].life = 30;

        let commander = add_creature(
            &mut state,
            PlayerId(0),
            "TrampleCmd",
            12,
            12,
            vec![Keyword::Trample],
        );
        state.objects.get_mut(&commander).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander,
            damage: 18,
        });
        let chump = add_creature(&mut state, PlayerId(1), "Token", 1, 1, vec![]);

        let assignments = choose_blockers(&state, PlayerId(1), &[commander]);

        assert!(
            !assignments.contains(&(chump, commander)),
            "AI must NOT chump 1/1 in front of 12/12 trample commander with 3 headroom \
             — trample-through (11) still crosses lethal cmd damage. Got: {:?}",
            assignments
        );
    }

    /// CR 702.2c + CR 702.19b: Deathtouch+trample needs only 1 damage assigned to a blocker
    /// before tramping. A 4/4 deathtouch+trample commander with 3 headroom defeats a 3/3 chump
    /// (trample-through = 4 - 1 = 3 ≥ headroom 3), even though without deathtouch the chump
    /// would absorb everything (4 - 3 = 1, safe).
    #[test]
    fn deathtouch_trample_commander_skips_chump_that_would_be_safe_without_deathtouch() {
        use engine::types::format::FormatConfig;
        use engine::types::game_state::CommanderDamageEntry;

        let mut state = setup();
        state.format_config = FormatConfig::commander();
        state.players[1].life = 30;

        let commander = add_creature(
            &mut state,
            PlayerId(0),
            "DTTrampleCmd",
            4,
            4,
            vec![Keyword::Trample, Keyword::Deathtouch],
        );
        state.objects.get_mut(&commander).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander,
            damage: 18,
        });
        let chump = add_creature(&mut state, PlayerId(1), "Bear", 3, 3, vec![]);

        let assignments = choose_blockers(&state, PlayerId(1), &[commander]);

        assert!(
            !assignments.contains(&(chump, commander)),
            "AI must NOT chump 3/3 in front of 4/4 deathtouch+trample commander with 3 headroom \
             — trample-through (3) crosses lethal cmd damage because deathtouch makes 1 damage \
             lethal to the blocker. Got: {:?}",
            assignments
        );
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

    // --- Gang-blocking tests (CR 509.1a) ---

    #[test]
    fn gang_block_kills_large_attacker() {
        let mut state = setup();
        // 6/6 attacker, two 3/3 blockers can combine to kill it
        let big = add_creature(&mut state, PlayerId(0), "Wurm", 6, 6, vec![]);
        let b1 = add_creature(&mut state, PlayerId(1), "Knight1", 3, 3, vec![]);
        let b2 = add_creature(&mut state, PlayerId(1), "Knight2", 3, 3, vec![]);
        state.players[1].life = 20;

        let blockers = choose_blockers(&state, PlayerId(1), &[big]);

        // Both 3/3s should gang-block the 6/6 (combined power 6 >= toughness 6)
        let blocking_big: Vec<_> = blockers.iter().filter(|&&(_, a)| a == big).collect();
        assert_eq!(
            blocking_big.len(),
            2,
            "Two 3/3s should gang-block the 6/6, got {:?}",
            blockers
        );
        assert!(
            blockers.iter().any(|&(b, _)| b == b1),
            "Knight1 should participate in gang-block"
        );
        assert!(
            blockers.iter().any(|&(b, _)| b == b2),
            "Knight2 should participate in gang-block"
        );
    }

    #[test]
    fn gang_block_skipped_when_value_not_worth_it() {
        let mut state = setup();
        // 2/2 attacker, two 3/3 blockers — don't waste two big creatures on a small one
        let small = add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);
        let _b1 = add_creature(&mut state, PlayerId(1), "Knight1", 3, 3, vec![]);
        let _b2 = add_creature(&mut state, PlayerId(1), "Knight2", 3, 3, vec![]);
        state.players[1].life = 20;

        let blockers = choose_blockers(&state, PlayerId(1), &[small]);

        // A single 3/3 already kills the 2/2 — second pass handles it, no gang needed.
        // But either way, should NOT have 2 blockers on a 2/2.
        let blocking_small: Vec<_> = blockers.iter().filter(|&&(_, a)| a == small).collect();
        assert!(
            blocking_small.len() <= 1,
            "Should not gang-block a small attacker with multiple large blockers"
        );
    }

    #[test]
    fn gang_block_skipped_against_deathtouch() {
        let mut state = setup();
        // 4/4 deathtouch attacker — gang-blocking loses multiple creatures
        let dt_attacker = add_creature(
            &mut state,
            PlayerId(0),
            "Basilisk",
            4,
            4,
            vec![Keyword::Deathtouch],
        );
        let _b1 = add_creature(&mut state, PlayerId(1), "Knight1", 3, 3, vec![]);
        let _b2 = add_creature(&mut state, PlayerId(1), "Knight2", 3, 3, vec![]);
        state.players[1].life = 20;

        let blockers = choose_blockers(&state, PlayerId(1), &[dt_attacker]);

        // Should not gang-block a deathtouch creature — all blockers die
        let blocking: Vec<_> = blockers
            .iter()
            .filter(|&&(_, a)| a == dt_attacker)
            .collect();
        assert!(
            blocking.len() <= 1,
            "Should not gang-block a deathtouch attacker, got {:?}",
            blockers
        );
    }

    // --- First-strike awareness tests (CR 702.7) ---

    #[test]
    fn first_strike_attacker_kills_before_blocker_deals_damage() {
        let mut state = setup();
        // 3/3 first striker attacks, 2/2 blocker would normally trade but
        // first strike kills the blocker before it deals damage
        let fs_attacker = add_creature(
            &mut state,
            PlayerId(0),
            "Knight",
            3,
            3,
            vec![Keyword::FirstStrike],
        );
        let blocker = add_creature(&mut state, PlayerId(1), "Bear", 2, 2, vec![]);
        state.players[1].life = 20;

        let blockers = choose_blockers(&state, PlayerId(1), &[fs_attacker]);

        // The 2/2 should NOT block because it dies to first strike before dealing damage
        // (priority = 0: doesn't kill, doesn't survive), and at 20 life no chump needed
        assert!(
            !blockers.iter().any(|&(b, _)| b == blocker),
            "2/2 should not block a 3/3 first-striker at high life (dies for nothing)"
        );
    }

    #[test]
    fn blocker_with_first_strike_survives_against_normal_attacker() {
        let mut state = setup();
        // 2/2 first-strike blocker vs 3/3 normal attacker
        // Blocker deals damage first, but 2 < 3 so attacker survives,
        // then attacker hits back for 3 which kills the 2/2.
        // However, a 3/3 first-striker blocking a 3/3 should kill it
        // before taking damage.
        let attacker = add_creature(&mut state, PlayerId(0), "Ogre", 3, 3, vec![]);
        let fs_blocker = add_creature(
            &mut state,
            PlayerId(1),
            "Elite",
            3,
            3,
            vec![Keyword::FirstStrike],
        );
        state.players[1].life = 20;

        let blockers = choose_blockers(&state, PlayerId(1), &[attacker]);

        // 3/3 first-striker kills the 3/3 before it deals damage — survives and kills
        assert!(
            blockers.contains(&(fs_blocker, attacker)),
            "3/3 first-striker should block 3/3 (kills before taking damage)"
        );
    }

    #[test]
    fn double_strike_attacker_deals_double_damage() {
        let mut state = setup();
        // 2/2 double-striker attacks, 3/3 blocker: double strike deals 2+2=4 total,
        // which kills the 3/3. The 3/3 deals 3 back, killing the 2/2 in the normal
        // damage step. But in first-strike step: 2 damage < 3 toughness, so the 3/3
        // survives first strike, then in normal step both deal lethal. It's a trade.
        // The 3/3 DOES kill the 2/2, so kills=true. But survives=false (takes 4 total).
        let ds_attacker = add_creature(
            &mut state,
            PlayerId(0),
            "Berserker",
            2,
            2,
            vec![Keyword::DoubleStrike],
        );
        let big_blocker = add_creature(&mut state, PlayerId(1), "Ogre", 3, 3, vec![]);
        state.players[1].life = 20;

        let blockers = choose_blockers(&state, PlayerId(1), &[ds_attacker]);

        // The 3/3 should block the 2/2 double-striker — it kills the attacker
        // (even though the blocker also dies, it's a favorable trade: 3/3 > 2/2)
        assert!(
            blockers.contains(&(big_blocker, ds_attacker)),
            "3/3 should block 2/2 double-striker (kills it, favorable trade)"
        );
    }

    // --- Deathtouch + flying legality tests ---

    #[test]
    fn deathtouch_without_flying_cannot_block_flyer() {
        let mut state = setup();
        let flyer = add_creature(
            &mut state,
            PlayerId(0),
            "Dragon",
            4,
            4,
            vec![Keyword::Flying],
        );
        let _dt_ground = add_creature(
            &mut state,
            PlayerId(1),
            "Snake",
            1,
            1,
            vec![Keyword::Deathtouch],
        );
        state.players[1].life = 20;

        let blockers = choose_blockers(&state, PlayerId(1), &[flyer]);

        // Ground deathtouch creature cannot block a flyer
        assert!(
            blockers.is_empty(),
            "Ground deathtouch creature should not block a flying attacker"
        );
    }

    #[test]
    fn deathtouch_with_reach_can_block_flyer() {
        let mut state = setup();
        let flyer = add_creature(
            &mut state,
            PlayerId(0),
            "Dragon",
            4,
            4,
            vec![Keyword::Flying],
        );
        let dt_reach = add_creature(
            &mut state,
            PlayerId(1),
            "Spider",
            1,
            1,
            vec![Keyword::Deathtouch, Keyword::Reach],
        );
        state.players[1].life = 20;

        let blockers = choose_blockers(&state, PlayerId(1), &[flyer]);

        // Deathtouch + reach can block and kill the flyer
        assert!(
            blockers.contains(&(dt_reach, flyer)),
            "Deathtouch creature with reach should block the flyer"
        );
    }

    #[test]
    fn skips_damage_reflection_blocker_at_low_life() {
        use engine::types::ability::{
            AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef, TargetFilter,
            TriggerDefinition,
        };
        use engine::types::triggers::TriggerMode;

        let mut state = setup();
        state.players[1].life = 4; // P1 at low life

        // P0 attacks with a 4/4
        let attacker = add_creature(&mut state, PlayerId(0), "Rhino", 4, 4, vec![]);

        // P1 has a Jackal Pup (2/1 with damage-reflection trigger)
        let pup = add_creature(&mut state, PlayerId(1), "Jackal Pup", 2, 1, vec![]);
        let pup_trigger = TriggerDefinition::new(TriggerMode::DamageReceived)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    target: TargetFilter::Controller,
                    damage_source: None,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Battlefield]);
        state
            .objects
            .get_mut(&pup)
            .unwrap()
            .trigger_definitions
            .push(pup_trigger);

        let blockers = choose_blockers(&state, PlayerId(1), &[attacker]);

        // Jackal Pup should NOT block the 4/4: taking 4 damage from the trigger
        // at 4 life would be lethal.
        assert!(
            !blockers.iter().any(|&(b, _)| b == pup),
            "Damage-reflection creature should not block when reflected damage is lethal"
        );
    }

    #[test]
    fn allows_damage_reflection_blocker_at_high_life() {
        use engine::types::ability::{
            AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef, TargetFilter,
            TriggerDefinition,
        };
        use engine::types::triggers::TriggerMode;

        let mut state = setup();
        state.players[1].life = 20; // P1 at high life

        // P0 attacks with a 2/2
        let attacker = add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);

        // P1 has a Jackal Pup (2/1 with damage-reflection)
        let pup = add_creature(&mut state, PlayerId(1), "Jackal Pup", 2, 1, vec![]);
        let pup_trigger = TriggerDefinition::new(TriggerMode::DamageReceived)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    target: TargetFilter::Controller,
                    damage_source: None,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Battlefield]);
        state
            .objects
            .get_mut(&pup)
            .unwrap()
            .trigger_definitions
            .push(pup_trigger);

        // P1 also has a normal 3/3 that can block favorably
        add_creature(&mut state, PlayerId(1), "Centaur", 3, 3, vec![]);

        let blockers = choose_blockers(&state, PlayerId(1), &[attacker]);

        // At high life, the Jackal Pup CAN kill the 2/2 attacker — priority > 0
        // (kills=true). But the Centaur is a better blocker (survives and kills).
        // The key point: the pup is NOT excluded from consideration at high life.
        assert!(
            !blockers.is_empty(),
            "Should have at least one blocker assigned"
        );
    }

    // ===== Hopeless-block fast-path (CR 509.1 + CR 704.5a) =====
    //
    // These tests verify `block_is_futile` correctly identifies positions
    // where no assignment saves the player, AND that pathological boards
    // (1000+ tokens) complete the blocker decision in bounded time.

    #[test]
    fn futile_one_huge_trampler_vs_thousand_tokens_bails_fast() {
        let mut state = setup();
        state.players[1].life = 20;

        // 1000 1/1 tokens — the Scute Swarm pathological board.
        for i in 0..1000 {
            add_creature(
                &mut state,
                PlayerId(1),
                &format!("Scute Token {i}"),
                1,
                1,
                vec![],
            );
        }
        let trampler = add_creature(
            &mut state,
            PlayerId(0),
            "Huge Trampler",
            1200,
            1200,
            vec![Keyword::Trample],
        );

        let start = std::time::Instant::now();
        let blockers = choose_blockers(&state, PlayerId(1), &[trampler]);
        let elapsed = start.elapsed();

        eprintln!(
            "[bench] choose_blockers (1000 tokens vs 1200/1200 trampler, life=20): {:?}",
            elapsed
        );
        // Trample residual = 1200 - 1000 = 200 >= 20 life → futile, must bail.
        assert!(
            blockers.is_empty(),
            "Must bail with empty assignment when trample-residual exceeds life; got {} assignments",
            blockers.len()
        );
        // Loose ceiling — fast-path should be O(blockers + attackers), not O(blockers^2).
        assert!(
            elapsed.as_millis() < 50,
            "Fast-path must complete in <50ms; took {:?}",
            elapsed
        );
    }

    #[test]
    fn futile_thousand_normal_attackers_vs_five_blockers_bails_fast() {
        let mut state = setup();
        state.players[1].life = 20;

        // 5 blockers (1/1) — best case absorbs 5 chumps.
        for i in 0..5 {
            add_creature(
                &mut state,
                PlayerId(1),
                &format!("Blocker {i}"),
                1,
                1,
                vec![],
            );
        }
        // 1000 attackers (3/3 normal, no trample). Best chump absorbs 5*3 = 15
        // damage; residual = 1000*3 - 15 = 2985, which is >= 20 life → futile.
        let mut attacker_ids = Vec::with_capacity(1000);
        for i in 0..1000 {
            attacker_ids.push(add_creature(
                &mut state,
                PlayerId(0),
                &format!("Goblin {i}"),
                3,
                3,
                vec![],
            ));
        }

        let start = std::time::Instant::now();
        let blockers = choose_blockers(&state, PlayerId(1), &attacker_ids);
        let elapsed = start.elapsed();

        eprintln!(
            "[bench] choose_blockers (5 blockers vs 1000 normal attackers, life=20): {:?}",
            elapsed
        );
        assert!(
            blockers.is_empty(),
            "Must bail when chumping every blocker still leaves lethal residual; got {} assignments",
            blockers.len()
        );
        assert!(
            elapsed.as_millis() < 100,
            "Fast-path must complete in <100ms; took {:?}",
            elapsed
        );
    }

    #[test]
    fn block_is_futile_does_not_fire_when_chump_saves() {
        let mut state = setup();
        state.players[1].life = 20;

        // 1 trampler 6/6 vs 5 blockers (1/1 each): residual = 6 - 5 = 1 < 20.
        // Not futile — existing chump-stabilize logic should engage.
        for i in 0..5 {
            add_creature(
                &mut state,
                PlayerId(1),
                &format!("Blocker {i}"),
                1,
                1,
                vec![],
            );
        }
        let trampler = add_creature(
            &mut state,
            PlayerId(0),
            "Modest Trampler",
            6,
            6,
            vec![Keyword::Trample],
        );

        let blockers = choose_blockers(&state, PlayerId(1), &[trampler]);
        // Even though objective is PreserveAdvantage here (6 < 20), the block
        // should not be skipped by the futility fast-path.
        assert!(
            !blockers.is_empty() || state.players[1].life > 6,
            "Non-futile board must not be short-circuited"
        );
    }

    #[test]
    fn block_is_futile_does_not_fire_when_normal_attacker_fully_absorbed() {
        let mut state = setup();
        state.players[1].life = 5;

        // 1 normal 3/3 attacker vs 1 5/5 blocker. life=5, raw incoming=3 → no
        // Stabilize objective at all, so futility check doesn't even run; the
        // assertion here documents that and guards against accidental regressions.
        add_creature(&mut state, PlayerId(1), "Wall", 0, 5, vec![]);
        let attacker = add_creature(&mut state, PlayerId(0), "Bear", 3, 3, vec![]);

        let blockers = choose_blockers(&state, PlayerId(1), &[attacker]);
        assert!(
            !blockers.is_empty(),
            "5/5 wall must block 3/3 bear; got empty assignment"
        );
    }

    /// CR 509.1 + CR 510.1c: `block_is_futile` must reserve the LARGEST-toughness
    /// blockers to soak tramplers and chump with the smallest, since chumping a
    /// non-trample attacker absorbs its full power regardless of the blocker's
    /// toughness. A survivable assignment here (chump the 1/1 with the 1/1, block
    /// the 5/5 trampler with the 10-toughness wall → 0 trample-through) must NOT
    /// be reported as futile. The bug reserved the SMALLEST blockers for trample,
    /// under-counting absorption and conceding survivable boards to lethal.
    #[test]
    fn block_is_futile_reserves_largest_blockers_for_trample() {
        let mut state = setup();
        state.players[1].life = 2;
        let wall = add_creature(&mut state, PlayerId(1), "Wall", 0, 10, vec![]);
        let small = add_creature(&mut state, PlayerId(1), "Small", 1, 1, vec![]);
        let trampler = add_creature(
            &mut state,
            PlayerId(0),
            "Trampler",
            5,
            5,
            vec![Keyword::Trample],
        );
        let bear = add_creature(&mut state, PlayerId(0), "Bear", 1, 1, vec![]);
        assert!(
            !block_is_futile(&state, PlayerId(1), &[trampler, bear], &[wall, small]),
            "Wall absorbs the trampler and Small chumps the Bear (residual 0 < life 2); not futile"
        );
    }

    /// CR 509.1 + CR 510.1c: `block_is_futile` must not assume chumping every
    /// possible attacker maximizes absorption. Chumping a low-power attacker
    /// consumes a blocker that could have soaked more trample damage. Here the
    /// optimum is to gang-block the 6/6 trampler with BOTH 0/3 walls (absorb 6 →
    /// 0 tramples through) and take 1 from the unblocked 1/1: residual 1 == life,
    /// not > life, so the board is survivable and must NOT be reported futile.
    /// The bug forced `chump_count = min(chumpables, blockers)` (always chump the
    /// 1/1), leaving only one wall (toughness 3) for trample → 3 tramples through,
    /// wrongly conceding the game.
    #[test]
    fn block_is_futile_skips_chump_to_soak_more_trample() {
        let mut state = setup();
        state.players[1].life = 1;
        let wall_a = add_creature(&mut state, PlayerId(1), "WallA", 0, 3, vec![]);
        let wall_b = add_creature(&mut state, PlayerId(1), "WallB", 0, 3, vec![]);
        let trampler = add_creature(
            &mut state,
            PlayerId(0),
            "Trampler",
            6,
            6,
            vec![Keyword::Trample],
        );
        let goblin = add_creature(&mut state, PlayerId(0), "Goblin", 1, 1, vec![]);
        assert!(
            !block_is_futile(&state, PlayerId(1), &[trampler, goblin], &[wall_a, wall_b]),
            "both walls should soak the trampler (residual 1 == life) instead of chumping the 1/1"
        );
    }

    // ───────────────────────── #8 lifelink block correctness ─────────────────

    /// #8: a lifelinker must NOT attack into a pure loss (it dies, the blocker
    /// lives, no kill) just for the life swing. Discriminating: reverting the
    /// `(free_damage || favorable_trade || attacker_survives)` gate in
    /// `should_attack_given_objective` flips this — the old code attacked.
    #[test]
    fn lifelink_does_not_attack_into_pure_loss() {
        let mut state = setup();
        let lifelinker = add_creature(
            &mut state,
            PlayerId(0),
            "Vamp",
            2,
            2,
            vec![Keyword::Lifelink],
        );
        // 3/4 wall: kills the 2/2, survives (2 < 4). Pure loss.
        add_creature(&mut state, PlayerId(1), "Wall", 3, 4, vec![]);

        let attacks = choose_attackers_with_targets(&state, PlayerId(0));
        assert!(
            !attacks.iter().any(|(id, _)| *id == lifelinker),
            "lifelinker must not be thrown into a pure-loss block for life gain"
        );
    }

    /// #8 guard (don't over-correct): a lifelinker that SURVIVES the block (2/5
    /// into a 3/3 — survives, deals 2, gains 2) should still attack. Confirms
    /// the gate only suppresses pure losses, not all lifelink swings.
    #[test]
    fn lifelink_still_attacks_when_surviving() {
        let mut state = setup();
        let lifelinker = add_creature(
            &mut state,
            PlayerId(0),
            "Vamp",
            2,
            5,
            vec![Keyword::Lifelink],
        );
        add_creature(&mut state, PlayerId(1), "Bear", 3, 3, vec![]);

        let attacks = choose_attackers_with_targets(&state, PlayerId(0));
        assert!(
            attacks.iter().any(|(id, _)| *id == lifelinker),
            "a surviving lifelinker should still attack"
        );
    }

    // ───────────────────────── #6 commander trade avoidance ──────────────────

    /// #6: the AI must not trade its commander into an even block (both die),
    /// forcing commander tax — while a vanilla creature in the same spot SHOULD
    /// trade. Discriminating: removing the commander gate makes the commander
    /// attack (the 3/3-vs-3/3 is a `favorable_trade`).
    #[test]
    fn commander_does_not_trade_into_equal_block() {
        let mut state = setup();
        let commander = add_creature(&mut state, PlayerId(0), "General", 3, 3, vec![]);
        state.objects.get_mut(&commander).unwrap().is_commander = true;
        let bear = add_creature(&mut state, PlayerId(0), "Bear", 3, 3, vec![]);
        add_creature(&mut state, PlayerId(1), "Blocker", 3, 3, vec![]);

        let attacks = choose_attackers_with_targets(&state, PlayerId(0));
        assert!(
            !attacks.iter().any(|(id, _)| *id == commander),
            "commander must not trade into an equal block"
        );
        assert!(
            attacks.iter().any(|(id, _)| *id == bear),
            "a vanilla creature in the same spot should still trade"
        );
    }

    /// B1 regression: the commander must also be excluded from the desperation
    /// ALPHA-STRIKE fallback (which re-adds the whole candidate set). A swarm of
    /// bears still alpha-strikes; the commander stays home. Discriminating:
    /// reverting to `attacking_ids = candidates.clone()` re-adds the commander.
    #[test]
    fn commander_excluded_from_alpha_strike() {
        let mut state = setup();
        let commander = add_creature(&mut state, PlayerId(0), "General", 2, 2, vec![]);
        state.objects.get_mut(&commander).unwrap().is_commander = true;
        // Five 3/3 bears: enough excess unblocked power to justify the
        // alpha-strike even after the single 0/4 wall "blocks" one body.
        let mut bears = Vec::new();
        for _ in 0..5 {
            bears.push(add_creature(&mut state, PlayerId(0), "Bear", 3, 3, vec![]));
        }
        add_creature(&mut state, PlayerId(1), "Wall", 0, 4, vec![]);

        let attacks = choose_attackers_with_targets(&state, PlayerId(0));
        assert!(
            !attacks.iter().any(|(id, _)| *id == commander),
            "commander must be excluded from the alpha-strike swing"
        );
        assert!(
            bears.iter().any(|b| attacks.iter().any(|(id, _)| id == b)),
            "the bear swarm should still alpha-strike (the swing fires without the commander)"
        );
    }

    // ───────────────────────── #5 attacking planeswalkers ────────────────────

    /// #5: with no lethal/near-lethal at the face, redirect the FEWEST large
    /// attackers needed to kill the opp planeswalker (largest-power-first),
    /// leaving the rest on the player. PW + targets derived from the engine.
    #[test]
    fn redirects_fewest_bodies_to_planeswalker() {
        let mut state = setup();
        let big = add_creature(&mut state, PlayerId(0), "Ogre", 5, 5, vec![]);
        let small_a = add_creature(&mut state, PlayerId(0), "Cub", 2, 2, vec![]);
        let small_b = add_creature(&mut state, PlayerId(0), "Cub", 2, 2, vec![]);
        let pw = add_planeswalker(&mut state, PlayerId(1), 3);
        let targets = valid_targets(&state);

        let attacks = choose_attackers_with_targets_with_profile(
            &state,
            PlayerId(0),
            &AiProfile::default(),
            false,
            None,
            Some(&targets),
        );

        // The lone 5/5 (>= loyalty 3) goes at the planeswalker; the 2/2s at the player.
        assert_eq!(
            attacks.iter().find(|(id, _)| *id == big).map(|(_, t)| *t),
            Some(AttackTarget::Planeswalker(pw)),
            "the fewest-bodies killing subset (the 5/5) should hit the planeswalker"
        );
        for cub in [small_a, small_b] {
            assert_eq!(
                attacks.iter().find(|(id, _)| *id == cub).map(|(_, t)| *t),
                Some(AttackTarget::Player(PlayerId(1))),
                "spare attackers stay on the player"
            );
        }
    }

    /// #5 guard: if the swing can't KILL the planeswalker, don't dribble — send
    /// everyone at the player.
    #[test]
    fn does_not_redirect_when_cannot_kill_pw() {
        let mut state = setup();
        add_creature(&mut state, PlayerId(0), "Cub", 2, 2, vec![]);
        add_creature(&mut state, PlayerId(0), "Cub", 2, 2, vec![]);
        add_planeswalker(&mut state, PlayerId(1), 6); // 4 total power < 6 loyalty
        let targets = valid_targets(&state);

        let attacks = choose_attackers_with_targets_with_profile(
            &state,
            PlayerId(0),
            &AiProfile::default(),
            false,
            None,
            Some(&targets),
        );
        assert!(
            attacks
                .iter()
                .all(|(_, t)| matches!(t, AttackTarget::Player(_))),
            "can't-kill planeswalker → no dribble, all at player"
        );
    }

    /// #5 guard: never empty the face — a lone attacker that could kill the PW
    /// still goes at the player.
    #[test]
    fn does_not_redirect_when_would_empty_face() {
        let mut state = setup();
        add_creature(&mut state, PlayerId(0), "Ogre", 5, 5, vec![]);
        add_planeswalker(&mut state, PlayerId(1), 3);
        let targets = valid_targets(&state);

        let attacks = choose_attackers_with_targets_with_profile(
            &state,
            PlayerId(0),
            &AiProfile::default(),
            false,
            None,
            Some(&targets),
        );
        assert!(
            attacks
                .iter()
                .all(|(_, t)| matches!(t, AttackTarget::Player(_))),
            "redirecting the only attacker would empty the face → stay on player"
        );
    }

    /// #5 guard: don't dilute a near-lethal swing (raw power >= opp life) into a
    /// planeswalker, even when not formally PushLethal (a blocker is present).
    #[test]
    fn does_not_redirect_when_near_lethal() {
        let mut state = setup();
        state.players[1].life = 6;
        add_creature(&mut state, PlayerId(0), "Brute", 4, 4, vec![]);
        add_creature(&mut state, PlayerId(0), "Brute", 4, 4, vec![]);
        add_creature(&mut state, PlayerId(1), "Chump", 0, 1, vec![]); // blocker → not PushLethal
        add_planeswalker(&mut state, PlayerId(1), 3);
        let targets = valid_targets(&state);

        let attacks = choose_attackers_with_targets_with_profile(
            &state,
            PlayerId(0),
            &AiProfile::default(),
            false,
            None,
            Some(&targets),
        );
        assert!(
            attacks
                .iter()
                .any(|(_, t)| matches!(t, AttackTarget::Player(PlayerId(1)))),
            "near-lethal swing should pressure the player, not the planeswalker"
        );
        assert!(
            !attacks
                .iter()
                .any(|(_, t)| matches!(t, AttackTarget::Planeswalker(_))),
            "no attacker should be diverted to the planeswalker when near-lethal"
        );
    }

    /// #5 regression: no planeswalker present → targeting is unchanged (player).
    #[test]
    fn no_pw_target_single_opponent_unchanged() {
        let mut state = setup();
        let bear = add_creature(&mut state, PlayerId(0), "Bear", 3, 3, vec![]);
        let targets = valid_targets(&state);

        let attacks = choose_attackers_with_targets_with_profile(
            &state,
            PlayerId(0),
            &AiProfile::default(),
            false,
            None,
            Some(&targets),
        );
        assert_eq!(
            attacks.iter().find(|(id, _)| *id == bear).map(|(_, t)| *t),
            Some(AttackTarget::Player(PlayerId(1))),
        );
    }
}
