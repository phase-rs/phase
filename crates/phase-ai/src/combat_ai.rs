use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use engine::ai_support::{adversarial_swarm_witness, SwarmWitnessResult};
use engine::game::combat::{
    can_block_pair, can_block_pair_with_precomputed, collect_block_restriction_statics,
    collect_blocker_allowed_statics, collect_blocker_restriction_statics, AttackTarget,
};
use engine::game::commander::commander_lethal_headroom;
use engine::game::players;
use engine::types::ability::StaticDefinition;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use crate::config::{AiConfig, AiProfile, ExecutionMode};
use crate::damage_reflection::has_damage_reflection_to_controller;
use crate::eval::{evaluate_creature, threat_level};
use crate::projection::{project_to, projection_deadline, Projection, ProjectionHorizon};
use crate::session::AiSession;

/// Block-legality static slices collected once per combat decision and threaded
/// through the per-pair `can_block_pair` checks. Hoisting these out of the
/// O(battlefield²) attacker/blocker loops avoids re-walking the battlefield's
/// functioning statics for every candidate pair.
pub(crate) struct BlockLegalitySlices {
    blocker_restriction: Vec<(ObjectId, StaticDefinition)>,
    block_restriction: Vec<(ObjectId, StaticDefinition)>,
    blocker_allowed: Vec<(ObjectId, StaticDefinition)>,
    // CR 604.1: shadow block-lift existence gate (CR 509.1b/609.4/702.28b),
    // hoisted once so per-pair legality skips the O(N) CanBlockShadow sweep.
    can_block_shadow_exists: bool,
}

impl BlockLegalitySlices {
    pub(crate) fn collect(state: &GameState) -> Self {
        Self {
            blocker_restriction: collect_blocker_restriction_statics(state),
            block_restriction: collect_block_restriction_statics(state),
            blocker_allowed: collect_blocker_allowed_statics(state),
            can_block_shadow_exists:
                engine::game::functioning_abilities::any_functioning_static_mode(state, |m| {
                    matches!(m, StaticMode::CanBlockShadow)
                }),
        }
    }

    /// CR 509.1a–b: per-pair block legality against the precomputed slices.
    pub(crate) fn can_block_pair(
        &self,
        state: &GameState,
        blocker_id: ObjectId,
        attacker_id: ObjectId,
    ) -> bool {
        can_block_pair_with_precomputed(
            state,
            blocker_id,
            attacker_id,
            &self.blocker_restriction,
            &self.block_restriction,
            &self.blocker_allowed,
            self.can_block_shadow_exists,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CombatObjective {
    PushLethal,
    Stabilize,
    PreserveAdvantage,
    Race,
}

/// Whether the attacker heuristic may spend an opponent-turn projection on
/// crackback analysis, and the execution regime that projection runs under.
///
/// The regime travels with the permission so the two cannot drift: a caller
/// cannot enable lookahead without stating a regime, and a caller that disables
/// it never has to invent one (`search::deterministic_combat_choice` has no
/// `AiConfig` at all). Replaces a `combat_lookahead: bool` that could express
/// only half the decision.
///
/// Carries `ExecutionMode`, NOT a `Deadline`, deliberately: a `Deadline`
/// snapshots an absolute instant at construction, and this value is built as an
/// argument at `search::deterministic_choice` — before this function's opponent
/// enumeration, must-attack sweep, `adversarial_swarm_witness` reducer replay,
/// block-legality collection and per-candidate `defender_best_block` loop have
/// run. Anchoring the 15 ms budget there would let that prologue consume it and
/// silently disable CEDH crackback lookahead on a large board. The `Deadline` is
/// therefore constructed at the point of use, below.
#[derive(Debug, Clone, Copy)]
pub enum CombatLookahead {
    Disabled,
    Enabled { execution_mode: ExecutionMode },
}

impl CombatLookahead {
    /// `AiConfig::combat_lookahead` decides permission; `execution_mode` travels
    /// with it so the measurement carve-out reaches the projection. Only CEDH
    /// enables this today, but `cargo ai-gate --difficulty cedh` is a supported
    /// invocation, so the carve-out must hold here too.
    pub fn from_config(config: &AiConfig) -> Self {
        if config.combat_lookahead {
            Self::Enabled {
                execution_mode: config.execution_mode,
            }
        } else {
            Self::Disabled
        }
    }
}

fn emit_attack_trace(
    player: PlayerId,
    candidate_attackers: &[ObjectId],
    assignments: &[(ObjectId, AttackTarget)],
) {
    if !tracing::event_enabled!(target: "phase_ai::decision_trace", tracing::Level::DEBUG) {
        return;
    }
    let chosen: Vec<String> = assignments
        .iter()
        .map(|(attacker, target)| format!("{attacker:?}->{target:?}"))
        .collect();
    let rejected: Vec<ObjectId> = candidate_attackers
        .iter()
        .copied()
        .filter(|id| !assignments.iter().any(|(attacker, _)| attacker == id))
        .collect();
    tracing::debug!(
        target: "phase_ai::decision_trace",
        ai_player = player.0,
        combat_kind = "attack",
        chosen = ?chosen,
        rejected = ?rejected,
        "combat decision"
    );
}

fn emit_block_trace(
    player: PlayerId,
    candidate_blockers: &[ObjectId],
    assignments: &[(ObjectId, ObjectId)],
) {
    if !tracing::event_enabled!(target: "phase_ai::decision_trace", tracing::Level::DEBUG) {
        return;
    }
    let chosen: Vec<String> = assignments
        .iter()
        .map(|(blocker, attacker)| format!("{blocker:?}->{attacker:?}"))
        .collect();
    let rejected: Vec<ObjectId> = candidate_blockers
        .iter()
        .copied()
        .filter(|id| !assignments.iter().any(|(blocker, _)| blocker == id))
        .collect();
    tracing::debug!(
        target: "phase_ai::decision_trace",
        ai_player = player.0,
        combat_kind = "block",
        chosen = ?chosen,
        rejected = ?rejected,
        "combat decision"
    );
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
        CombatLookahead::Disabled,
        None,
        None,
        None,
    )
}

pub fn choose_attackers_with_targets_with_profile(
    state: &GameState,
    player: PlayerId,
    profile: &AiProfile,
    lookahead: CombatLookahead,
    valid_attacker_ids: Option<&[ObjectId]>,
    valid_attack_targets: Option<&[AttackTarget]>,
    session: Option<&AiSession>,
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
    // Loop-invariant hoist: `attackable_player_targets` depends only on `state`
    // (immutable during this filter), so compute it once instead of per creature
    // inside `creature_must_attack`.
    let attackable = engine::game::combat::attackable_player_targets(state);
    let mandatory: Vec<ObjectId> = candidates
        .iter()
        .copied()
        .filter(|&id| {
            engine::game::combat::creature_must_attack_with_attackable_players(
                state,
                id,
                &attackable,
            )
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

    // CR 508.1 / CR 509.1 / CR 510.1: promote only an exact lethal
    // declaration the engine has reducer-replayed against every bounded legal
    // defense. This precedes all value, objective, crackback, and redirection
    // policy gates: each would otherwise mutate the certified action.
    // Preserve the non-mandatory commander heuristic, but include every
    // must-attack creature before certification so the result cannot authorize
    // a later union.
    let alpha_candidates: Vec<ObjectId> = candidates
        .iter()
        .copied()
        .filter(|&id| {
            !state
                .objects
                .get(&id)
                .map(|object| object.is_commander)
                .unwrap_or(false)
        })
        .collect();
    let mut certified_candidates = alpha_candidates;
    for &id in &mandatory {
        if !certified_candidates.contains(&id) {
            certified_candidates.push(id);
        }
    }
    if state.players.len() == 2 && opponents.len() == 1 {
        let certified_attacks: Vec<_> = certified_candidates
            .iter()
            .map(|&id| (id, AttackTarget::Player(opponents[0])))
            .collect();
        if matches!(
            adversarial_swarm_witness(state, player, &certified_attacks),
            SwarmWitnessResult::Certified(witness)
                if witness.is_lethal && witness.binds_declaration(state, &certified_attacks)
        ) {
            emit_attack_trace(player, &candidates, &certified_attacks);
            return certified_attacks;
        }
    }

    let objective = determine_attack_objective(
        state,
        player,
        &opponents,
        &candidates,
        &opponent_blockers,
        profile,
    );

    // Hoist the block-legality static slices once for the whole candidate sweep —
    // `defender_best_block` runs an O(blockers) `can_block_pair` filter per
    // candidate, so collecting these per call would re-walk the battlefield's
    // statics O(candidates × blockers) times.
    let slices = BlockLegalitySlices::collect(state);

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
        match defender_best_block(state, id, my_value, &opponent_blockers, &slices) {
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
        let projection: Option<Arc<Projection>> = match lookahead {
            CombatLookahead::Disabled => None,
            CombatLookahead::Enabled { execution_mode } => {
                // Constructed HERE, not at the caller: `Deadline::after` snapshots
                // an absolute instant, and everything above in this function
                // (must-attack sweep, adversarial_swarm_witness reducer replay,
                // block-legality slices, per-candidate defender_best_block) would
                // otherwise run inside the 15 ms projection budget.
                let deadline = projection_deadline(execution_mode);
                match session {
                    // Session present: route through the per-game projection cache
                    // (turn-scoped key; identical result to project_to on a miss,
                    // cached on subsequent identical combat decisions this turn).
                    Some(session) => session
                        .get_or_project(
                            state,
                            player,
                            opponents[0],
                            ProjectionHorizon::OpponentAttackersDeclared,
                            deadline,
                        )
                        .ok(),
                    // No session: the planner's production quiescence loop, the
                    // public `choose_attackers_with_targets` wrapper, and tests.
                    // Fall back to the free projection, wrapped in Arc to unify
                    // the branch type.
                    None => project_to(
                        state,
                        player,
                        opponents[0],
                        ProjectionHorizon::OpponentAttackersDeclared,
                        deadline,
                    )
                    .ok()
                    .map(Arc::new),
                }
            }
        };
        let cb_damage = crackback_damage(
            state,
            player,
            &opponents,
            &attacking_ids,
            projection.as_deref(),
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
                    crackback_damage(state, player, &opponents, &remaining, projection.as_deref());
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
        let assignments = redirect_attackers_to_planeswalker(
            state,
            &attacking_ids,
            valid_attack_targets,
            objective,
            opp,
            opponent_life,
        );
        emit_attack_trace(player, &candidates, &assignments);
        return assignments;
    }

    // Multi-opponent: assign attack targets (planeswalker redirect deferred).
    let assignments = assign_attack_targets(state, player, &opponents, attacking_ids);
    emit_attack_trace(player, &candidates, &assignments);
    assignments
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
    // CR 509.1a: `used_blockers` / `blocked_attackers` are membership indices over
    // the assignment set, hot on large boards (token swarms) where the per-pass
    // `Vec::contains` / `iter().any()` scans were O(blockers²) / O(attackers ·
    // assignments). HashSet lookups make them O(1); the produced assignments are
    // identical because neither set is ever iterated, only membership-tested.
    let mut used_blockers: HashSet<ObjectId> = HashSet::new();
    let mut blocked_attackers: HashSet<ObjectId> = HashSet::new();
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
        emit_block_trace(player, &available_blockers, &[]);
        return Vec::new();
    }

    let blocker_values: HashMap<ObjectId, f64> = available_blockers
        .iter()
        .map(|&id| (id, evaluate_creature(state, id)))
        .collect();
    let blocker_value = |id: &ObjectId| -> f64 { blocker_values.get(id).copied().unwrap_or(0.0) };

    // CR 509.1b + CR 702.111b: the minimum number of creatures that must block a
    // given attacker. Menace is only the most common source of that floor — a
    // `MinBlockers` restriction ("can't be blocked except by three or more
    // creatures", e.g. Pathrazer of Ulamog) imposes an arbitrary one, and every
    // pass below must respect it or the declaration it builds is illegal and gets
    // rewritten to the empty witness by `complete_blocker_proposal` (issue #7183).
    //
    // `min_blockers_required` is the engine's single authority for the floor — the
    // same value `validate_blockers` enforces and `block_requirements_for_player`
    // shows the UI — so the AI can never plan against a rule different from the one
    // enforced. Read through it rather than through `block_requirements_for_player`
    // because that helper derives from `state.combat`, which callers that evaluate
    // a hypothetical block (lookahead, tests) may not have populated; the floor is
    // a property of the attacker's own restrictions, not of the combat record.
    //
    // The block-restriction statics are collected once here and threaded through
    // the precomputed variant: every pass below reads this per attacker, and the
    // non-precomputed entry point re-walks the battlefield on each call.
    let block_restriction_statics = engine::game::combat::collect_block_restriction_statics(state);
    let required_blockers = |attacker_id: &ObjectId| -> usize {
        engine::game::combat::min_blockers_required_from_precomputed(
            state,
            *attacker_id,
            &block_restriction_statics,
        ) as usize
    };

    // Sort attackers by value (highest first) to prioritize blocking high-value threats
    let mut sorted_attackers: Vec<(ObjectId, f64)> = attacker_ids
        .iter()
        .map(|&id| (id, evaluate_creature(state, id)))
        .collect();
    sorted_attackers.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // First pass: assign deathtouch blockers to highest-value attackers.
    // CR 509.1b: Skip attackers with a minimum-blocker floor above 1 — a lone
    // blocker is an illegal declaration against them (handled in gang-block pass).
    for &(attacker_id, _) in &sorted_attackers {
        if !state.objects.contains_key(&attacker_id) {
            continue;
        }
        if required_blockers(&attacker_id) > 1 {
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
            used_blockers.insert(blocker_id);
            blocked_attackers.insert(attacker_id);
        }
    }

    // Second pass: assign remaining blockers where they'd survive.
    // CR 509.1b: Skip attackers with a minimum-blocker floor above 1 — a lone
    // blocker is an illegal declaration against them (handled in gang-block pass).
    for &(attacker_id, attacker_value) in &sorted_attackers {
        if blocked_attackers.contains(&attacker_id) {
            continue; // Already blocked
        }

        let attacker = match state.objects.get(&attacker_id) {
            Some(a) => a,
            None => continue,
        };
        if required_blockers(&attacker_id) > 1 {
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

        if let Some((blocker_id, priority, selected_blocker_value)) = best {
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
            let chump_unsafe =
                priority == 0 && commander_chump_unsafe(state, player, attacker_id, &[blocker_id]);
            let favorable_trade =
                priority != 1 || selected_blocker_value <= attacker_value + damage_prevented as f64;
            if !chump_unsafe
                && ((priority > 0 && favorable_trade)
                    || should_chump_stabilize
                    || should_chump_race)
            {
                assignments.push((blocker_id, attacker_id));
                used_blockers.insert(blocker_id);
                blocked_attackers.insert(attacker_id);
            }
        }
    }

    // Gang-blocking pass (CR 509.1a): assign multiple blockers to a single attacker
    // when no single blocker can kill it but combined power can.
    // Only gang-block when the combined blocker value is less than the attacker value.
    for &(attacker_id, attacker_value) in &sorted_attackers {
        if blocked_attackers.contains(&attacker_id) {
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
        let attacker_has_trample = attacker.has_keyword(&Keyword::Trample);

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
        // CR 509.1b: Exception — an attacker with a minimum-blocker floor MUST be
        // gang-blocked even when a single blocker could kill it, because a lone
        // block is an illegal declaration.
        let needed_blockers = required_blockers(&attacker_id);
        if needed_blockers <= 1
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

        // CR 509.1b + CR 903.10a: the lethal-pressure test for the survival override
        // below. Hoisted above the two value heuristics that follow because both of
        // them short-circuit this attacker out of the pass entirely, and whether
        // that is correct depends on whether a block is the player's last line.
        let p_life = state.players[player.0 as usize].life;
        let effective_life = commander_lethal_headroom(state, player, attacker_id)
            .map(|headroom| p_life.min(headroom as i32))
            .unwrap_or(p_life);

        // CR 509.1b: an attacker whose minimum-blocker floor exceeds 1 is skipped by
        // both single-blocker passes, so this gang pass is its only blocking route.
        let floor_stabilize_route = needed_blockers > 1
            && matches!(objective, CombatObjective::Stabilize)
            && effective_life <= attacker_power * 3;

        // CR 510.1c: a blocked creature assigns its combat damage to the creatures
        // blocking it, and "if no creatures are currently blocking it (if, for
        // example, they were destroyed or removed from combat), it assigns no combat
        // damage." So for a *nontrampling* attacker a legal block prevents every
        // point of damage to the player whether or not the blockers survive it and
        // whether or not they can kill the attacker — a doomed block is still a full
        // save. Trample is the exception (CR 702.19b): excess damage past lethal to
        // the blockers is assigned to the player, and under deathtouch "lethal" is
        // only 1 per blocker (CR 702.2c), so a trampler's damage still lands.
        //
        // The two heuristics below are value heuristics — correct when the question
        // is "is this trade worth it", wrong when the question is "do I survive".
        // They may only decline a block that is not the difference between living
        // and losing (issue #7183).
        let survival_route_is_live = floor_stabilize_route;

        // CR 702.7b: If attacker has first strike and blocker doesn't, the blocker
        // dies before dealing damage. Skip blockers that would die to first strike —
        // they contribute no damage, so they cannot be counted toward a kill.
        let effective_candidates: Vec<(ObjectId, i32, f64)> = gang_candidates
            .iter()
            .copied()
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
        // gang-blocking just loses more creatures — unless the block is the
        // player's only route to surviving the turn, where CR 510.1c makes the
        // doomed block a full save anyway.
        if attacker_has_deathtouch && !survival_route_is_live {
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

        // CR 509.1b: The declaration needs at least `needed_blockers` creatures on
        // this attacker. Top the gang set up to that floor even when combined power
        // already suffices — a short set is an illegal declaration, not a cheaper
        // one. Loops rather than adding a single blocker: menace's floor is 2, but a
        // `MinBlockers` restriction can require any number (Pathrazer of Ulamog
        // requires 3), so one top-up is not enough (issue #7183).
        // Damage-dealing candidates come first so the kill claim below stays honest.
        // Only when a doomed block is still a full save (CR 510.1c) may the floor be
        // filled out with blockers that die to first strike before dealing damage
        // (CR 702.7b) — otherwise a legal, life-saving declaration is impossible to
        // reach for a floored first- or double-striker at all.
        while gang_set.len() < needed_blockers {
            let next = effective_candidates
                .iter()
                .find(|(bid, _, _)| !gang_set.contains(bid))
                .copied()
                .or_else(|| {
                    if !survival_route_is_live {
                        return None;
                    }
                    // Contributes no damage, so it is added with zero power: it pads
                    // the CR 509.1b floor without inflating `combined_power`.
                    gang_candidates
                        .iter()
                        .find(|(bid, _, _)| !gang_set.contains(bid))
                        .map(|&(bid, _, value)| (bid, 0, value))
                });
            let Some((bid, power, value)) = next else {
                break;
            };
            combined_power += power;
            gang_set.push(bid);
            gang_value += value;
        }

        // The engine owns this calculation. `combat_damage_to_defender` models both
        // damage steps (CR 702.4b), the blockers a first striker removes between them
        // (CR 702.7b), per-blocker lethal minimums including damage already marked
        // (CR 702.19b), a blocked trampler left with no blockers (CR 702.19d), and
        // deathtouch (CR 702.2c). Every one of those interacts, and the AI's previous
        // hand-rolled single-step estimate got the double-strike case wrong by a
        // whole second strike.
        let damage_through = |set: &[ObjectId]| -> i32 {
            engine::game::combat_damage::combat_damage_to_defender(state, attacker_id, set)
        };
        // The no-block baseline, read from the same authority so the comparison below
        // is like for like. NOT `attacker_power`: that is one damage step's worth,
        // while `damage_through` reports the whole combat phase, so a double striker
        // makes the two disagree by an entire strike (CR 702.4b) — and disagree in
        // the direction that rejects a block which does save the game.
        let unblocked_damage = damage_through(&[]);
        // CR 510.1c: a block that lets nothing through is a full save whatever
        // becomes of the blockers. Otherwise it has to get the player under lethal
        // AND actually prevent something worth the creatures — the same
        // `damage_prevented >= 2` floor the single-blocker pass applies.
        let averts_lethal = |set: &[ObjectId]| -> bool {
            let through = damage_through(set);
            through < effective_life && (through == 0 || unblocked_damage - through >= 2)
        };

        // CR 509.1b + CR 510.1c + CR 702.7b + CR 702.19b: the survival gang answers a
        // different question from the kill gang, so it draws from a different pool.
        // EVERY legal blocker absorbs, including one the attacker kills in the
        // first-strike step — trample still has to assign that blocker its lethal
        // damage before excess reaches the player, and a nontrampler assigns nothing
        // to the player once blocked whatever becomes of its blockers. Filtering the
        // first-strike casualties out is right for a kill estimate and wrong here: it
        // left an 11/11 menace first-strike trampler unblockable by two 4/4s at 4
        // life, though that block absorbs 4+4 and tramples only 3.
        //
        // Ordering: against a trampler what a blocker contributes is absorption, not
        // cheapness, so take the biggest absorbers first and break ties on value —
        // a value-ordered walk spends several 1/1s where one 6/6 would do. Against a
        // nontrampler any legal block already prevents everything (CR 510.1c), so the
        // cheapest bodies are correct there and the existing value order stands.
        let survival_gang: Option<Vec<ObjectId>> = if floor_stabilize_route {
            let mut pool: Vec<(ObjectId, f64)> = gang_candidates
                .iter()
                .map(|&(bid, _, value)| (bid, value))
                .collect();
            if attacker_has_trample {
                pool.sort_by(|a, b| {
                    let absorb = |bid: ObjectId| {
                        engine::game::combat_damage::lethal_damage_needed(
                            state,
                            bid,
                            attacker_has_deathtouch,
                        )
                    };
                    absorb(b.0)
                        .cmp(&absorb(a.0))
                        .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                });
            }
            let mut set: Vec<ObjectId> = Vec::new();
            for &(bid, _) in &pool {
                if set.len() >= needed_blockers && averts_lethal(&set) {
                    break;
                }
                set.push(bid);
            }

            // The walk above adds by *static* absorption, which cannot see what a
            // blocker contributes beyond soaking damage — a first-striking
            // deathtouch blocker can kill the attacker outright in the first step
            // (CR 702.7b + CR 702.2c) and cancel its regular-step damage entirely.
            // So once the set survives, drop any member the rest has made redundant,
            // judged through the same whole-combat authority rather than the
            // ordering heuristic. One bounded pass over the gang, never below the
            // CR 509.1b floor.
            //
            // Concretely: a 10/10 menace double-strike trampler against two 0/6
            // walls and a 1/1 first-strike deathtouch blocker. Absorption order
            // takes both walls (8 still gets through) and then the 1/1 to survive —
            // but one wall plus the 1/1 holds it to 3, because the attacker dies
            // before the regular step. Three creatures spent where two suffice.
            let mut index = 0;
            while index < set.len() && set.len() > needed_blockers {
                let mut trial = set.clone();
                trial.remove(index);
                if averts_lethal(&trial) {
                    set = trial;
                } else {
                    index += 1;
                }
            }

            (set.len() >= needed_blockers && averts_lethal(&set)).then_some(set)
        } else {
            None
        };

        // CR 509.1b + CR 704.5a: Survival override, restricted to attackers that a
        // minimum-blocker floor routes here. Such an attacker is skipped by BOTH
        // single-blocker chump passes, which are the only places carrying a
        // survival override (`should_chump_stabilize`), so this gang pass is its
        // sole blocking route — and the kill-and-trade gates below are a pure value
        // heuristic that declines a legal, life-saving block. That is how a menace /
        // "can't be blocked except by three or more creatures" attacker (Pathrazer
        // of Ulamog) walks past a full board for lethal (issue #7183).
        //
        // Deliberately NOT applied when the floor is 1: those attackers already ran
        // through the single-blocker passes with their full guard set, so widening
        // the gate here would double-count their chump decision.
        //
        // Mirrors the single-blocker pass guard-for-guard: the same
        // `effective_life <= attacker_power * 3` threshold with the CR 903.10a
        // commander-damage tightening, the same `commander_chump_unsafe` rejection
        // (evaluated against the gang's absorption, since trample assigns lethal
        // damage to every blocker before any tramples through, CR 702.19b), and the
        // same exclusion of damage-reflection blockers, which hand the attacker's
        // power straight back to the player and so save nothing.
        let reflects_damage = |set: &[ObjectId]| -> bool {
            set.iter().any(|bid| {
                state
                    .objects
                    .get(bid)
                    .is_some_and(has_damage_reflection_to_controller)
            })
        };

        // CR 702.2c: never gang a deathtouch attacker for *value* — every blocker
        // assigned any damage dies, so the kill is paid for with the whole gang.
        // Preserves the pre-existing skip for deathtouch attackers now that the
        // survival route no longer short-circuits them out of the pass.
        let gang_kills_for_value = !attacker_has_deathtouch
            && combined_power >= attacker_toughness
            && gang_value <= attacker_value
            && gang_set.len() >= needed_blockers
            && !reflects_damage(&gang_set);

        // The survival route already proved its own set legal and lethal-averting;
        // it only remains to reject the two shapes that save nothing.
        let stabilizing_gang = survival_gang.filter(|set| {
            !reflects_damage(set) && !commander_chump_unsafe(state, player, attacker_id, set)
        });

        // Gang-block to kill when the trade is worth it, else to survive. Never below
        // the CR 509.1b floor, which would make the declaration illegal.
        let declared_gang = if gang_kills_for_value {
            Some(gang_set)
        } else {
            stabilizing_gang
        };
        if let Some(gang_set) = declared_gang {
            for bid in gang_set {
                assignments.push((bid, attacker_id));
                used_blockers.insert(bid);
            }
            blocked_attackers.insert(attacker_id);
        }
    }

    // Third pass: if unblocked damage is still lethal, greedily assign remaining
    // blockers to the highest-power unblocked attackers to survive.
    if matches!(objective, CombatObjective::Stabilize) {
        let p_life = state.players[player.0 as usize].life;
        let unblocked_damage: i32 = sorted_attackers
            .iter()
            .filter(|&&(aid, _)| !blocked_attackers.contains(&aid))
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
                .filter(|&&(aid, _)| !blocked_attackers.contains(&aid))
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
                // CR 509.1b: Skip attackers with a minimum-blocker floor above 1 —
                // a single chump block is an illegal declaration against them.
                if required_blockers(&attacker_id) > 1 {
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
                    used_blockers.insert(blocker_id);
                    blocked_attackers.insert(attacker_id);
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
            if blocked_attackers.contains(&attacker_id) {
                continue; // Already blocked
            }
            let attacker = match state.objects.get(&attacker_id) {
                Some(a) => a,
                None => continue,
            };
            // CR 509.1b: A minimum-blocker floor above 1 is handled by the
            // gang-block pass, not by a single-creature chump.
            if required_blockers(&attacker_id) > 1 {
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
                !commander_chump_unsafe(state, player, attacker_id, &[bid])
            });
            if let Some(&blocker_id) = safe_blocker {
                assignments.push((blocker_id, attacker_id));
                used_blockers.insert(blocker_id);
                blocked_attackers.insert(attacker_id);
            }
            // No safe chump exists — accept the loss on this commander rather than
            // wasting a creature that won't actually save the player. Continue to
            // the next commander attacker so other independent threats can still chump.
        }
    }

    emit_block_trace(player, &available_blockers, &assignments);
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
    blockers: &[ObjectId],
) -> bool {
    let Some(headroom) = commander_lethal_headroom(state, defender, attacker_id) else {
        return false;
    };
    // CR 903.10a: unsafe when what still gets through would cross the
    // commander-damage threshold. Delegates to the engine's damage authority rather
    // than re-deriving trample and deathtouch here — the previous local version took
    // a single blocker's toughness and replaced it with 1 under deathtouch (CR
    // 702.2c), which is right for one chump blocker and throws away a whole gang's
    // absorption when handed one, rejecting legal blocks that do prevent lethality.
    engine::game::combat_damage::combat_damage_to_defender(state, attacker_id, blockers) as u32
        >= headroom
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
    // Hoist block-legality statics once for the greedy O(attackers × blockers)
    // assignment sweep below. `attacker_source` is the only state queried.
    let slices = BlockLegalitySlices::collect(attacker_source);
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
            if !slices.can_block_pair(attacker_source, bid, opp_id) {
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

    // CR 508.1c + CR 611.2c: respect an active additional-combat attacker
    // restriction (Last Night Together / Bumi). Hardens this hypothetical/test
    // fallback path; at runtime the AI consumes the engine's pre-filtered
    // valid_attacker_ids, and validate_attackers remains the ultimate gate.
    if !engine::game::combat::passes_combat_attacker_restriction(state, obj_id) {
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
    slices: &BlockLegalitySlices,
) -> Option<DefenderBlock> {
    let attacker = state.objects.get(&attacker_id)?;
    blockers
        .iter()
        .filter(|&&bid| slices.can_block_pair(state, bid, attacker_id))
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
    use crate::planner::quick_state_hash;
    use crate::projection::ProjectionKey;
    use engine::game::zones::create_object;
    use engine::types::game_state::WaitingFor;
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

    /// Item E (revert-failing perf): the must-attack partition computes the
    /// attackable-player set ONCE, so the number of `attackable_player_targets`
    /// sweeps does NOT scale with the goaded-creature count. Pre-fix each
    /// goaded creature's `creature_must_attack` recomputed it, so the sweep count
    /// grew with K.
    fn goaded_attacker_sweep_count(num_goaded: usize) -> u64 {
        let mut state = setup();
        state.phase = engine::types::phase::Phase::DeclareAttackers;
        for _ in 0..num_goaded {
            let id = add_creature(&mut state, PlayerId(0), "Goaded", 2, 2, vec![]);
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .goaded_by
                .insert(PlayerId(1));
        }
        engine::game::perf_counters::reset();
        let _ = choose_attackers(&state, PlayerId(0));
        engine::game::perf_counters::snapshot().attackable_player_sweeps
    }

    #[test]
    fn attacker_choice_sweeps_attackable_players_independent_of_goaded_count() {
        let one = goaded_attacker_sweep_count(1);
        let many = goaded_attacker_sweep_count(4);
        assert!(
            one >= 1,
            "the must-attack partition must actually sweep (non-degenerate fixture)"
        );
        assert_eq!(
            one, many,
            "attackable-player sweeps must not scale with goaded count \
             (revert-failing: pre-fix grows as K)"
        );
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

    /// A 1/1 that would normally NOT attack into a 5/5 blocker is still chosen
    /// when it carries an unblockable static — the `is_unblockable` short-circuit
    /// fires before `defender_best_block`. This guards that the hoisted
    /// `BlockLegalitySlices` path leaves the unblockable-attacker decision
    /// byte-for-byte identical to the pre-hoist behavior. Reverted-fix
    /// discrimination: if the slices threading broke the unblockable detection or
    /// the block sweep, this attacker would be (wrongly) skipped like the plain
    /// 1/1 in `skips_unprofitable_attack`.
    #[test]
    fn unblockable_attacker_still_chosen_with_static_restriction() {
        use engine::types::ability::StaticDefinition;

        let mut state = setup();
        let small = add_creature(&mut state, PlayerId(0), "Squirrel", 1, 1, vec![]);
        state
            .objects
            .get_mut(&small)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeBlocked));
        add_creature(&mut state, PlayerId(1), "Giant", 5, 5, vec![]);

        let attackers = choose_attackers(&state, PlayerId(0));
        assert!(
            attackers.contains(&small),
            "an unblockable 1/1 must still attack into a 5/5 blocker"
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
    fn valuable_blocker_does_not_trade_down_into_small_deathtouch_attacker() {
        let mut state = setup();
        state.players[1].life = 20;
        let snake = add_creature(
            &mut state,
            PlayerId(0),
            "Snake Token",
            1,
            1,
            vec![Keyword::Deathtouch],
        );
        let sam = add_creature(
            &mut state,
            PlayerId(1),
            "Sam, Loyal Attendant",
            3,
            3,
            vec![],
        );

        let blockers = choose_blockers(&state, PlayerId(1), &[snake]);

        assert!(
            !blockers
                .iter()
                .any(|&(blocker, attacker)| blocker == sam && attacker == snake),
            "AI should not trade a valuable blocker down into a 1/1 deathtouch attacker at 20 life"
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
                    excess: None,
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
                    excess: None,
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
        state.players[PlayerId(1).0 as usize].life = 10;
        state.phase = engine::types::phase::Phase::DeclareAttackers;
        state.waiting_for = engine::game::combat::build_declare_attackers_waiting_for(&state);

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

    /// Production-path guard: the policy must promote a reducer-certified lethal
    /// alpha strike when the attacker count equals the defender's creature count,
    /// but only one defender can block the flyers. The engine-built
    /// `DeclareAttackers` payload is essential here: the witness replays this
    /// actual declaration and the chosen action is then accepted by the same
    /// reducer.
    ///
    /// CR 508.1 / CR 509.1 / CR 510.1b-c: three 1/1 flyers into one reach
    /// creature and two ground creatures at two life leave two unblocked damage.
    /// The equal raw counts must not reject this legal, lethal declaration before
    /// the witness checks actual block legality.
    #[test]
    fn reducer_certified_equal_count_flying_swarm_promotes_alpha() {
        let mut scenario = engine::game::scenario::GameScenario::new();
        scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
        let flyers: Vec<_> = (0..3)
            .map(|_| {
                scenario
                    .add_creature(PlayerId(0), "Flyer", 1, 1)
                    .flying()
                    .id()
            })
            .collect();
        scenario.add_creature(PlayerId(1), "Reach", 0, 2).reach();
        for _ in 0..2 {
            scenario.add_creature(PlayerId(1), "Ground", 0, 2);
        }
        scenario.with_life(PlayerId(1), 2);
        let mut runner = scenario.build();
        runner.advance_to_combat();

        let (valid_attacker_ids, valid_attack_targets) = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers {
                player,
                valid_attacker_ids,
                valid_attack_targets,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                (valid_attacker_ids.clone(), valid_attack_targets.clone())
            }
            other => panic!("fixture must reach DeclareAttackers, got {other:?}"),
        };
        let alpha_attacks: Vec<_> = flyers
            .iter()
            .copied()
            .map(|id| (id, AttackTarget::Player(PlayerId(1))))
            .collect();
        let result = adversarial_swarm_witness(runner.state(), PlayerId(0), &alpha_attacks);
        let SwarmWitnessResult::Certified(witness) = result else {
            panic!("equal-count flying alpha must certify: {result:?}");
        };
        assert!(witness.is_lethal);
        assert_eq!(witness.resulting_life_loss, 2);
        assert!(witness.binds_declaration(runner.state(), &alpha_attacks));

        let attacks = choose_attackers_with_targets_with_profile(
            runner.state(),
            PlayerId(0),
            &AiProfile::default(),
            CombatLookahead::Disabled,
            Some(&valid_attacker_ids),
            Some(&valid_attack_targets),
            None,
        );
        assert_eq!(
            attacks, alpha_attacks,
            "only the engine Certified(lethal) witness may promote this hostile alpha strike"
        );
        runner
            .declare_attackers(&attacks)
            .expect("the policy's witness-backed DeclareAttackers action must be reducer-legal");
    }

    /// Phase 5 production-path guard: the policy must promote a reducer-certified
    /// lethal alpha strike even when the legacy creature-value comparison would
    /// reject it. The engine-built `DeclareAttackers` payload is essential here:
    /// the witness replays this actual declaration and the chosen action is then
    /// accepted by the same reducer.
    ///
    /// CR 508.1 / CR 509.1 / CR 510.1b-c: three 3/3s into one 5/5 at five
    /// life leave six life loss after the defender's best legal block. Each
    /// individual bear is a pure loss, and the old fallback's `6 > 7.5` gate
    /// would incorrectly decline the lethal attack.
    #[test]
    fn reducer_certified_swarm_lethal_overrides_legacy_trade_value_veto() {
        let mut scenario = engine::game::scenario::GameScenario::new();
        scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
        let bears: Vec<_> = (0..3)
            .map(|_| scenario.add_creature(PlayerId(0), "Bear", 3, 3).id())
            .collect();
        scenario.add_creature(PlayerId(1), "Giant", 5, 5);
        scenario.with_life(PlayerId(1), 5);
        let mut runner = scenario.build();
        runner.advance_to_combat();

        let (valid_attacker_ids, valid_attack_targets) = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers {
                player,
                valid_attacker_ids,
                valid_attack_targets,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                (valid_attacker_ids.clone(), valid_attack_targets.clone())
            }
            other => panic!("fixture must reach DeclareAttackers, got {other:?}"),
        };
        let alpha_attacks: Vec<_> = bears
            .iter()
            .copied()
            .map(|id| (id, AttackTarget::Player(PlayerId(1))))
            .collect();
        assert!(matches!(
            adversarial_swarm_witness(runner.state(), PlayerId(0), &alpha_attacks),
            SwarmWitnessResult::Certified(witness) if witness.is_lethal
        ));
        assert!(
            6.0 <= evaluate_creature(runner.state(), bears[0]),
            "fixture must defeat the retired unblocked-power-versus-sacrifice-value gate"
        );

        let attacks = choose_attackers_with_targets_with_profile(
            runner.state(),
            PlayerId(0),
            &AiProfile::default(),
            CombatLookahead::Disabled,
            Some(&valid_attacker_ids),
            Some(&valid_attack_targets),
            None,
        );
        assert_eq!(
            attacks, alpha_attacks,
            "only the engine Certified(lethal) witness may promote this hostile alpha strike"
        );
        runner
            .declare_attackers(&attacks)
            .expect("the policy's witness-backed DeclareAttackers action must be reducer-legal");
    }

    /// A mandatory commander must be part of the declaration before it is
    /// certified. This hostile commander has trample, so the complete action is
    /// indeterminate and the policy must not promote the optional bears.
    #[test]
    fn swarm_certificate_is_revoked_when_mandatory_union_changes_the_action() {
        let mut scenario = engine::game::scenario::GameScenario::new();
        scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
        let commander = scenario
            .add_creature(PlayerId(0), "Goaded Commander", 3, 3)
            .trample()
            .id();
        let bears: Vec<_> = (0..3)
            .map(|_| scenario.add_creature(PlayerId(0), "Bear", 3, 3).id())
            .collect();
        scenario.add_creature(PlayerId(1), "Giant", 5, 5);
        scenario.with_life(PlayerId(1), 5);
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&commander)
            .expect("fixture commander")
            .is_commander = true;
        runner
            .state_mut()
            .objects
            .get_mut(&commander)
            .expect("fixture commander")
            .goaded_by
            .insert(PlayerId(1));
        runner.advance_to_combat();

        let (valid_attacker_ids, valid_attack_targets) = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers {
                valid_attacker_ids,
                valid_attack_targets,
                ..
            } => (valid_attacker_ids.clone(), valid_attack_targets.clone()),
            other => panic!("fixture must reach DeclareAttackers, got {other:?}"),
        };
        let complete_action: Vec<_> =
            std::iter::once((commander, AttackTarget::Player(PlayerId(1))))
                .chain(
                    bears
                        .iter()
                        .copied()
                        .map(|id| (id, AttackTarget::Player(PlayerId(1)))),
                )
                .collect();
        assert!(matches!(
            adversarial_swarm_witness(runner.state(), PlayerId(0), &complete_action),
            SwarmWitnessResult::Indeterminate(
                engine::ai_support::SwarmWitnessIndeterminate::DamageChoice
            )
        ));

        assert_eq!(
            choose_attackers_with_targets_with_profile(
                runner.state(),
                PlayerId(0),
                &AiProfile::default(),
                CombatLookahead::Disabled,
                Some(&valid_attacker_ids),
                Some(&valid_attack_targets),
                None,
            ),
            vec![(commander, AttackTarget::Player(PlayerId(1)))],
            "the mandatory attacker remains legal, but an indeterminate complete declaration must not promote optional bears"
        );
    }

    /// A lethal certificate must bypass crackback pruning. Holding back one
    /// bear would make the future swing survivable, but it would no longer be
    /// the exact declaration that the engine replayed.
    #[test]
    fn swarm_certificate_is_not_pruned_for_crackback() {
        let mut scenario = engine::game::scenario::GameScenario::new();
        scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
        let bears: Vec<_> = (0..3)
            .map(|_| scenario.add_creature(PlayerId(0), "Bear", 3, 3).id())
            .collect();
        scenario.add_creature(PlayerId(1), "Giant", 5, 5);
        scenario.with_life(PlayerId(0), 5);
        scenario.with_life(PlayerId(1), 5);
        let mut runner = scenario.build();
        runner.advance_to_combat();

        let (valid_attacker_ids, valid_attack_targets) = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers {
                valid_attacker_ids,
                valid_attack_targets,
                ..
            } => (valid_attacker_ids.clone(), valid_attack_targets.clone()),
            other => panic!("fixture must reach DeclareAttackers, got {other:?}"),
        };
        let certified_attacks: Vec<_> = bears
            .iter()
            .copied()
            .map(|id| (id, AttackTarget::Player(PlayerId(1))))
            .collect();
        assert!(matches!(
            adversarial_swarm_witness(runner.state(), PlayerId(0), &certified_attacks),
            SwarmWitnessResult::Certified(witness) if witness.is_lethal
        ));
        assert!(
            crackback_damage(runner.state(), PlayerId(0), &[PlayerId(1)], &bears, None) >= 5,
            "fixture must activate the crackback-pruning branch"
        );

        assert_eq!(
            choose_attackers_with_targets_with_profile(
                runner.state(),
                PlayerId(0),
                &AiProfile::default(),
                CombatLookahead::Disabled,
                Some(&valid_attacker_ids),
                Some(&valid_attack_targets),
                None,
            ),
            certified_attacks,
            "crackback must not remove an attacker from a certified declaration"
        );
    }

    /// Player-target binding is part of the certificate. Double strike makes
    /// this exact declaration reducer-lethal even though its raw power is below
    /// the opponent's life, so the redirect helper would otherwise divert it to
    /// a planeswalker. The certified player attack must be returned unchanged.
    #[test]
    fn swarm_certificate_keeps_its_player_targets_despite_planeswalker_redirect() {
        let mut scenario = engine::game::scenario::GameScenario::new();
        scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
        let bears: Vec<_> = (0..3)
            .map(|_| {
                scenario
                    .add_creature(PlayerId(0), "Double-strike Bear", 3, 3)
                    .double_strike()
                    .id()
            })
            .collect();
        scenario.add_creature(PlayerId(1), "Giant", 7, 7);
        scenario.with_life(PlayerId(1), 10);
        let mut runner = scenario.build();
        let planeswalker = add_planeswalker(runner.state_mut(), PlayerId(1), 3);
        runner.advance_to_combat();

        let (valid_attacker_ids, valid_attack_targets) = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers {
                valid_attacker_ids,
                valid_attack_targets,
                ..
            } => (valid_attacker_ids.clone(), valid_attack_targets.clone()),
            other => panic!("fixture must reach DeclareAttackers, got {other:?}"),
        };
        assert!(valid_attack_targets.contains(&AttackTarget::Planeswalker(planeswalker)));
        let certified_attacks: Vec<_> = bears
            .iter()
            .copied()
            .map(|id| (id, AttackTarget::Player(PlayerId(1))))
            .collect();
        assert!(matches!(
            adversarial_swarm_witness(runner.state(), PlayerId(0), &certified_attacks),
            SwarmWitnessResult::Certified(witness)
                if witness.is_lethal && witness.resulting_life_loss == 12
        ));
        assert!(
            bears
                .iter()
                .filter_map(|id| runner.state().objects.get(id)?.power)
                .sum::<i32>()
                < runner.state().players[PlayerId(1).0 as usize].life,
            "the redirect path must remain live for this exact certified declaration"
        );
        let redirected = redirect_attackers_to_planeswalker(
            runner.state(),
            &bears,
            Some(&valid_attack_targets),
            CombatObjective::PreserveAdvantage,
            PlayerId(1),
            runner.state().players[PlayerId(1).0 as usize].life,
        );
        assert!(
            redirected
                .iter()
                .any(|(_, target)| *target == AttackTarget::Planeswalker(planeswalker)),
            "the redirect transform must mutate this exact certified declaration"
        );
        assert_ne!(
            redirected, certified_attacks,
            "revert guard: post-certificate redirection would invalidate the certificate"
        );

        assert_eq!(
            choose_attackers_with_targets_with_profile(
                runner.state(),
                PlayerId(0),
                &AiProfile::default(),
                CombatLookahead::Disabled,
                Some(&valid_attacker_ids),
                Some(&valid_attack_targets),
                None,
            ),
            certified_attacks,
            "planeswalker redirection must not mutate the certified player-target declaration"
        );
    }

    /// Companion fail-closed guard: the same hostile board must not attack when
    /// a trample attacker makes the witness indeterminate due to damage-assignment
    /// choice. This proves the policy consumes the typed engine fact rather than
    /// independently treating raw excess power as forced lethal.
    #[test]
    fn swarm_alpha_abstains_when_engine_witness_is_indeterminate() {
        let mut scenario = engine::game::scenario::GameScenario::new();
        scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
        let trampler = scenario
            .add_creature(PlayerId(0), "Trampling Bear", 3, 3)
            .trample()
            .id();
        let bears = [
            trampler,
            scenario.add_creature(PlayerId(0), "Bear", 3, 3).id(),
            scenario.add_creature(PlayerId(0), "Bear", 3, 3).id(),
        ];
        scenario.add_creature(PlayerId(1), "Giant", 5, 5);
        scenario.with_life(PlayerId(1), 5);
        let mut runner = scenario.build();
        runner.advance_to_combat();

        let (valid_attacker_ids, valid_attack_targets) = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers {
                valid_attacker_ids,
                valid_attack_targets,
                ..
            } => (valid_attacker_ids.clone(), valid_attack_targets.clone()),
            other => panic!("fixture must reach DeclareAttackers, got {other:?}"),
        };
        let alpha_attacks: Vec<_> = bears
            .iter()
            .copied()
            .map(|id| (id, AttackTarget::Player(PlayerId(1))))
            .collect();
        assert!(matches!(
            adversarial_swarm_witness(runner.state(), PlayerId(0), &alpha_attacks),
            SwarmWitnessResult::Indeterminate(
                engine::ai_support::SwarmWitnessIndeterminate::DamageChoice
            )
        ));

        assert!(
            choose_attackers_with_targets_with_profile(
                runner.state(),
                PlayerId(0),
                &AiProfile::default(),
                CombatLookahead::Disabled,
                Some(&valid_attacker_ids),
                Some(&valid_attack_targets),
                None,
            )
            .is_empty(),
            "without a Certified(lethal) engine witness, the hostile alpha strike must abstain"
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
            CombatLookahead::Disabled,
            None,
            Some(&targets),
            None,
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
            CombatLookahead::Disabled,
            None,
            Some(&targets),
            None,
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
            CombatLookahead::Disabled,
            None,
            Some(&targets),
            None,
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
            CombatLookahead::Disabled,
            None,
            Some(&targets),
            None,
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
            CombatLookahead::Disabled,
            None,
            Some(&targets),
            None,
        );
        assert_eq!(
            attacks.iter().find(|(id, _)| *id == bear).map(|(_, t)| *t),
            Some(AttackTarget::Player(PlayerId(1))),
        );
    }

    // --- Session projection routing (perf pipeline 3) ---

    /// Deterministic "already-at-horizon" fixture: the opponent (P1) is the
    /// active player, sitting at priority with an attacker already declared and
    /// an empty stack, so `project_to`'s already-at-horizon short-circuit
    /// returns `Confidence::Exact` with no simulation and no wall-clock
    /// dependence. P0 has a lone 2-power attacker (etb turn 1 ⇒ can_attack) and
    /// P1 has no untapped blockers, so the entry point reaches the crackback
    /// projection block (opponent_blockers empty ⇒ attacker pushed ⇒ objective
    /// is not PushLethal).
    fn session_projection_fixture() -> GameState {
        let mut state = setup();
        state.active_player = PlayerId(1);
        let attacker = add_creature(&mut state, PlayerId(0), "Bear", 2, 2, vec![]);
        // creatures_attacked_this_turn is a HashSet — reached_horizon only
        // checks it is non-empty, so any ObjectId satisfies the predicate.
        state.creatures_attacked_this_turn.insert(attacker);
        state.stack.clear();
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };
        state.players[1].life = 20;
        state
    }

    /// Test A (revert-failing): with `combat_lookahead` on and a session
    /// present, the combat projection is routed through `get_or_project`, which
    /// populates the per-game cache under the exact turn-scoped key. Reverting
    /// to the free `project_to` leaves the cache empty and flips both asserts.
    #[test]
    fn session_projection_populates_cache_with_exact_key() {
        let state = session_projection_fixture();
        let session = AiSession::empty();
        let profile = AiProfile::default();

        let _ = choose_attackers_with_targets_with_profile(
            &state,
            PlayerId(0),
            &profile,
            /* lookahead = */
            CombatLookahead::Enabled {
                execution_mode: ExecutionMode::Interactive,
            },
            None,
            None,
            Some(&session),
        );

        let expected = ProjectionKey {
            state_hash: quick_state_hash(&state),
            turn_number: state.turn_number,
            active_player: state.active_player,
            ai_player: PlayerId(0),
            target_opponent: PlayerId(1),
            horizon: ProjectionHorizon::OpponentAttackersDeclared,
        };
        let cache = session.projection_cache.read().unwrap();
        assert_eq!(
            cache.len(),
            1,
            "combat_lookahead projection must populate exactly one cache entry \
             (revert-failing: free project_to caches nothing)"
        );
        assert!(
            cache.contains_key(&expected),
            "the cached projection must be keyed by the exact turn-scoped ProjectionKey"
        );
    }

    /// Test A2 (positive reach-guard for Test A): the session is consulted
    /// only when `combat_lookahead` is on. With it off, the same fixture leaves
    /// the cache empty — proving Test A's non-empty cache is caused by the
    /// lookahead routing, not by any incidental fixture side effect.
    #[test]
    fn session_projection_skipped_when_lookahead_off() {
        let state = session_projection_fixture();
        let session = AiSession::empty();
        let profile = AiProfile::default();

        let _ = choose_attackers_with_targets_with_profile(
            &state,
            PlayerId(0),
            &profile,
            /* lookahead = */ CombatLookahead::Disabled,
            None,
            None,
            Some(&session),
        );

        assert!(
            session.projection_cache.read().unwrap().is_empty(),
            "with combat_lookahead off, no projection is taken and the cache stays empty"
        );
    }

    /// Test B (behavior-neutral): routing the combat projection through the
    /// session cache produces the identical attacker decision as the free
    /// `project_to` path. This passes on reverted code too — by design — and
    /// guards against a semantic drift in the caching refactor.
    #[test]
    fn session_projection_decision_neutral_vs_free() {
        let state = session_projection_fixture();
        let profile = AiProfile::default();

        let with_session = choose_attackers_with_targets_with_profile(
            &state,
            PlayerId(0),
            &profile,
            /* lookahead = */
            CombatLookahead::Enabled {
                execution_mode: ExecutionMode::Interactive,
            },
            None,
            None,
            Some(&AiSession::empty()),
        );
        let without_session = choose_attackers_with_targets_with_profile(
            &state,
            PlayerId(0),
            &profile,
            /* lookahead = */
            CombatLookahead::Enabled {
                execution_mode: ExecutionMode::Interactive,
            },
            None,
            None,
            None,
        );

        assert_eq!(
            with_session, without_session,
            "session-cached projection must yield the identical attacker decision as the free path"
        );
    }

    /// T5a + T5b — `CombatLookahead::from_config` binds two authorities into one
    /// value, and each must survive the binding.
    ///
    /// T5a (regime survives): the CEDH preset is the only one enabling combat
    /// lookahead, so it is the only config that can carry a regime here. Its
    /// measurement variant must arrive as `Enabled { execution_mode }` with the
    /// measurement regime intact — `from_config` hardcoding
    /// `ExecutionMode::Interactive` turns this red, and that is exactly the bug
    /// that would leave the gate reading the wall clock at `--difficulty cedh`.
    ///
    /// T5b (permission dominates): with `combat_lookahead == false` the result
    /// is `Disabled` in BOTH regimes, so measurement mode cannot smuggle a
    /// projection into a tier that never takes one.
    #[test]
    fn combat_lookahead_from_config_carries_execution_mode() {
        use crate::config::{create_config, AiDifficulty, Platform};

        // T5a.
        let cedh = create_config(AiDifficulty::CEDH, Platform::Native);
        assert!(
            cedh.combat_lookahead,
            "T5a precondition: CEDH must be the tier that enables combat lookahead — if this \
             preset changes, this test is measuring the wrong config"
        );
        let measured = CombatLookahead::from_config(&cedh.clone().into_measurement(1));
        assert!(
            matches!(
                measured,
                CombatLookahead::Enabled { execution_mode } if execution_mode.is_measurement()
            ),
            "a measurement CEDH config must produce Enabled carrying the measurement regime; \
             got {measured:?}"
        );
        let interactive = CombatLookahead::from_config(&cedh);
        assert!(
            matches!(
                interactive,
                CombatLookahead::Enabled { execution_mode } if !execution_mode.is_measurement()
            ),
            "an interactive CEDH config must produce Enabled carrying the interactive regime; \
             got {interactive:?}"
        );

        // T5b — the permission axis dominates in both regimes.
        let medium = create_config(AiDifficulty::Medium, Platform::Native);
        assert!(
            !medium.combat_lookahead,
            "T5b precondition: Medium must NOT enable combat lookahead — a future preset flip \
             must fail loudly here rather than pass silently"
        );
        assert!(
            matches!(
                CombatLookahead::from_config(&medium),
                CombatLookahead::Disabled
            ),
            "combat_lookahead == false must map to Disabled in interactive mode"
        );
        assert!(
            matches!(
                CombatLookahead::from_config(&medium.into_measurement(1)),
                CombatLookahead::Disabled
            ),
            "combat_lookahead == false must map to Disabled in measurement mode too — \
             measurement must not enable a projection the tier never takes"
        );
    }

    // ── CR 509.1b minimum-blocker floors (issue #7183) ────────────────────────
    //
    // These exercise the *class* — any attacker whose minimum-blocker floor
    // exceeds 1 — through both of its sources: the Menace keyword (CR 702.111b,
    // floor 2) and a `CantBeBlockedExceptBy { MinBlockers { min } }` static
    // (CR 509.1b, arbitrary floor). Before the fix the AI keyed on the Menace
    // keyword alone, so a `MinBlockers` attacker was routed through the
    // single-blocker passes, produced an illegal lone block, and had it rewritten
    // to the empty declaration by `complete_blocker_proposal` — the AI never
    // blocked Pathrazer of Ulamog even at lethal.

    /// Give `attacker` a "can't be blocked except by `min` or more creatures"
    /// restriction — the static half of the CR 509.1b floor, as parsed from
    /// Pathrazer of Ulamog's second line.
    fn add_min_blockers_restriction(state: &mut GameState, attacker: ObjectId, min: u32) {
        use engine::types::ability::StaticDefinition;
        use engine::types::statics::BlockExceptionKind;

        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
                kind: BlockExceptionKind::MinBlockers { min },
            }));
    }

    /// Board where `blockers` 4/4s face one lethal attacker at `life`.
    fn lethal_attacker_board(life: i32, blockers: usize) -> (GameState, ObjectId, Vec<ObjectId>) {
        lethal_attacker_board_with(life, blockers, vec![])
    }

    /// Same board, with `keywords` on the 11/11 attacker. Parameterized rather than
    /// duplicated so each floor source (Menace / `MinBlockers`) can be crossed with
    /// each combat-damage keyword class (deathtouch, first strike, trample).
    fn lethal_attacker_board_with(
        life: i32,
        blockers: usize,
        keywords: Vec<Keyword>,
    ) -> (GameState, ObjectId, Vec<ObjectId>) {
        let mut state = setup();
        state.players[1].life = life;
        let attacker = add_creature(&mut state, PlayerId(0), "Attacker", 11, 11, keywords);
        let blocker_ids = (0..blockers)
            .map(|i| {
                add_creature(
                    &mut state,
                    PlayerId(1),
                    &format!("Blocker {i}"),
                    4,
                    4,
                    vec![],
                )
            })
            .collect();
        (state, attacker, blocker_ids)
    }

    /// CR 509.1b: an attacker with a `MinBlockers { min: 3 }` restriction must be
    /// gang-blocked by at least three creatures. The AI is at 6 life facing 11
    /// power, so declining loses the game outright while a legal three-creature
    /// block is available.
    #[test]
    fn min_blockers_three_attacker_is_gang_blocked_at_lethal() {
        let (mut state, attacker, _) = lethal_attacker_board(6, 10);
        add_min_blockers_restriction(&mut state, attacker, 3);

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert!(
            on_attacker >= 3,
            "a 'can't be blocked except by three or more creatures' attacker must be \
             gang-blocked by at least 3 at lethal, got {assignments:?}"
        );
        // The declaration the engine would actually accept — proves the proposal is
        // legal rather than silently rewritten to the empty witness.
        assert_eq!(
            engine::game::combat::complete_blocker_proposal(&state, PlayerId(1), &assignments),
            engine::types::actions::GameAction::DeclareBlockers {
                assignments: assignments.clone()
            },
            "the AI's declaration must survive the CR 509.1c completion authority"
        );
    }

    /// CR 702.111b: the Menace floor of 2 is the same class and must behave
    /// identically — the fix must not be specific to the static form.
    #[test]
    fn menace_attacker_is_gang_blocked_at_lethal() {
        let (mut state, attacker, _) = lethal_attacker_board(6, 10);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Menace);

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert!(
            on_attacker >= 2,
            "a menace attacker must be gang-blocked by at least 2 at lethal, got {assignments:?}"
        );
    }

    /// A floor above 1 must never produce a *short* declaration. With only two
    /// creatures available against a `MinBlockers { min: 3 }` attacker no legal
    /// block exists, so the AI must decline rather than offer an illegal pair
    /// (which the completion authority would rewrite to empty anyway).
    #[test]
    fn min_blockers_floor_is_never_under_filled() {
        let (mut state, attacker, _) = lethal_attacker_board(6, 2);
        add_min_blockers_restriction(&mut state, attacker, 3);

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert!(
            on_attacker == 0 || on_attacker >= 3,
            "a declaration against a min-3 attacker is either absent or at least 3 \
             creatures — never a short, illegal set. Got {assignments:?}"
        );
    }

    /// The survival override is scoped to lethal pressure: at a comfortable life
    /// total the AI must still decline an unprofitable gang block rather than
    /// throwing three 4/4s at a 5/5. Guards against the fix over-blocking.
    #[test]
    fn min_blockers_attacker_is_not_gang_blocked_when_not_under_pressure() {
        let mut state = setup();
        state.players[1].life = 40;
        let attacker = add_creature(&mut state, PlayerId(0), "Attacker", 5, 5, vec![]);
        add_min_blockers_restriction(&mut state, attacker, 3);
        for i in 0..10 {
            add_creature(
                &mut state,
                PlayerId(1),
                &format!("Blocker {i}"),
                4,
                4,
                vec![],
            );
        }

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        assert!(
            assignments.is_empty(),
            "at 40 life a 5/5 is not worth three 4/4s — the survival override must not \
             fire outside Stabilize. Got {assignments:?}"
        );
    }

    // ── CR 510.1c doomed-but-saving floor blocks (issue #7183 review) ─────────
    //
    // A floored attacker reaches the battlefield-clearing gang pass as its ONLY
    // blocking route. Two value heuristics live on that route — the CR 702.7b
    // first-strike filter and the CR 702.2c deathtouch skip — and both used to
    // fire before the survival override, so a menace/min-3 attacker with either
    // keyword walked past a full board for lethal even though CR 510.1c makes the
    // block a complete save: a blocked nontrampling attacker assigns its damage to
    // its blockers, and none to the player even when every blocker is already dead.
    //
    // Each test below is discriminating: reverting its half of the fix (the
    // `!doomed_block_still_saves` guard on the deathtouch skip, or the doomed
    // top-up pool behind the first-strike filter) drops the block to zero.

    /// Nothing the gang does can save the blockers from an 11/11 deathtouch
    /// attacker, but CR 510.1c means the block still prevents all 11 damage.
    /// At 6 life that is the difference between living and losing.
    #[test]
    fn min_blockers_three_deathtouch_attacker_is_gang_blocked_at_lethal() {
        let (mut state, attacker, _) = lethal_attacker_board_with(6, 10, vec![Keyword::Deathtouch]);
        add_min_blockers_restriction(&mut state, attacker, 3);

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert!(
            on_attacker >= 3,
            "CR 510.1c: a min-3 deathtouch attacker must still be gang-blocked at \
             lethal — the blockers all die, but the player takes 0. Got {assignments:?}"
        );
        assert_eq!(
            engine::game::combat::complete_blocker_proposal(&state, PlayerId(1), &assignments),
            engine::types::actions::GameAction::DeclareBlockers {
                assignments: assignments.clone()
            },
            "the AI's declaration must survive the CR 509.1c completion authority"
        );
    }

    /// Same class through the Menace floor of 2 (CR 702.111b).
    #[test]
    fn menace_deathtouch_attacker_is_gang_blocked_at_lethal() {
        let (state, attacker, _) =
            lethal_attacker_board_with(6, 10, vec![Keyword::Menace, Keyword::Deathtouch]);

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert!(
            on_attacker >= 2,
            "CR 510.1c: a menace deathtouch attacker must still be gang-blocked at \
             lethal. Got {assignments:?}"
        );
    }

    /// CR 702.7b: every 4/4 blocker dies in the first-strike step before dealing
    /// damage, so the first-strike filter empties the candidate set. CR 510.1c
    /// still makes the block a full save — the attacker has no blockers left to
    /// assign to, and no trample to push damage through to the player.
    #[test]
    fn min_blockers_three_first_strike_attacker_is_gang_blocked_at_lethal() {
        let (mut state, attacker, _) =
            lethal_attacker_board_with(6, 10, vec![Keyword::FirstStrike]);
        add_min_blockers_restriction(&mut state, attacker, 3);

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert!(
            on_attacker >= 3,
            "CR 510.1c: a min-3 first-striker must still be gang-blocked at lethal \
             even though no blocker survives to deal damage. Got {assignments:?}"
        );
        assert_eq!(
            engine::game::combat::complete_blocker_proposal(&state, PlayerId(1), &assignments),
            engine::types::actions::GameAction::DeclareBlockers {
                assignments: assignments.clone()
            },
            "the AI's declaration must survive the CR 509.1c completion authority"
        );
    }

    /// Double strike takes the same route (CR 702.4b): the blockers die in the
    /// first-strike step, and CR 510.1c leaves the attacker with nothing to assign
    /// to in the regular step.
    #[test]
    fn menace_double_strike_attacker_is_gang_blocked_at_lethal() {
        let (state, attacker, _) =
            lethal_attacker_board_with(6, 10, vec![Keyword::Menace, Keyword::DoubleStrike]);

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert!(
            on_attacker >= 2,
            "CR 510.1c: a menace double-striker must still be gang-blocked at lethal. \
             Got {assignments:?}"
        );
    }

    /// CR 702.19b + CR 702.2c: a deathtouch trampler assigns only 1 damage per
    /// blocker as "lethal", so each blocker absorbs 1 and the rest tramples through.
    /// With ten 4/4s available against an 11/11 at 6 life the AI must still block —
    /// six blockers absorb 6 and leave 5, surviving at 1 — and must spend exactly
    /// the six that achieve it rather than the whole board.
    ///
    /// The absorption is what decides, not the trample keyword: an earlier revision
    /// refused every deathtouch trampler outright, which declines a block that
    /// saves the game.
    #[test]
    fn menace_deathtouch_trampler_is_ganged_only_as_far_as_survival_needs() {
        let (state, attacker, _) = lethal_attacker_board_with(
            6,
            10,
            vec![Keyword::Menace, Keyword::Deathtouch, Keyword::Trample],
        );

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert_eq!(
            on_attacker, 6,
            "CR 702.2c: deathtouch makes 1 damage lethal, so N blockers absorb N. \
             At 6 life facing 11 power the AI needs 6 (residual 5) and must not \
             spend more. Got {assignments:?}"
        );
    }

    /// The other side of that boundary: when absorption cannot get under the life
    /// total the block is a pure loss and must be declined. Three 4/4s absorb only
    /// 3 against a deathtouch trampler, leaving 8 against 6 life.
    ///
    /// Note `block_is_futile` does not catch this — it bounds absorption by raw
    /// toughness (12 here) and so believes the board survives. The gang gate's
    /// `lethal_damage_needed` accounting is what declines it.
    #[test]
    fn menace_deathtouch_trampler_is_declined_when_absorption_cannot_save() {
        let (state, attacker, _) = lethal_attacker_board_with(
            6,
            3,
            vec![Keyword::Menace, Keyword::Deathtouch, Keyword::Trample],
        );

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert_eq!(
            on_attacker, 0,
            "three blockers absorb 3 under deathtouch, leaving 8 against 6 life — \
             the gang dies and the player still dies. Got {assignments:?}"
        );
    }

    /// CR 702.7b + CR 702.19b: a first-striker kills these blockers before their
    /// damage step, so they are excluded from the *kill* estimate — but trample must
    /// still assign each of them its lethal damage before any excess reaches the
    /// player. An 11/11 menace first-strike trampler against two 4/4s at 4 life
    /// assigns 4+4 and tramples 3, leaving the player alive at 1.
    ///
    /// Regression for deriving the survival gang from `effective_candidates`: that
    /// filter emptied the pool, so this legal, life-saving block could not be formed
    /// at all.
    #[test]
    fn menace_first_strike_trampler_is_gang_blocked_by_doomed_absorbers() {
        let (state, attacker, _) = lethal_attacker_board_with(
            4,
            2,
            vec![Keyword::Menace, Keyword::FirstStrike, Keyword::Trample],
        );

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert_eq!(
            on_attacker, 2,
            "CR 702.19b: blockers that die in the first-strike step still absorb \
             their lethal damage — 4+4 of 11 leaves 3 against 4 life. Got \
             {assignments:?}"
        );
    }

    /// CR 702.4b + CR 702.19d: a double striker assigns damage in BOTH steps. The
    /// first step kills these blockers and tramples 3; the second finds no blockers
    /// and, per CR 702.19d, assigns its full 11 to the player "as though all
    /// blocking creatures have been assigned lethal damage". 14 total against 4
    /// life, so the block does not save and must be declined.
    ///
    /// Discriminating for the single-step model: counting one step reads this as
    /// 3 through and approves a block that loses the game.
    #[test]
    fn menace_double_strike_trampler_is_declined_when_the_second_strike_still_kills() {
        let (state, attacker, _) = lethal_attacker_board_with(
            4,
            2,
            vec![Keyword::Menace, Keyword::DoubleStrike, Keyword::Trample],
        );

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert_eq!(
            on_attacker, 0,
            "CR 702.4b + CR 702.19d: the second strike finds an empty block and \
             tramples 11 more into a player already down to 1. Got {assignments:?}"
        );
    }

    /// The survival test must compare like with like. `combat_damage_to_defender`
    /// reports the WHOLE combat phase, so for a double striker it counts both steps
    /// (CR 702.4b) while `attacker_power` is one step's worth — and the block that
    /// saves the game is exactly where the two disagree.
    ///
    /// A 5/5 menace double-strike trampler is 10 damage unblocked (5 through the
    /// first step's excess plus 5 more into the empty block, CR 702.19d), lethal at
    /// 7 life. Two 2/2s absorb 2+2 in the first step, so 1 tramples then, and 5 in
    /// the regular step: 6 total, and the player lives at 1.
    ///
    /// Comparing against raw `attacker_power` gives `5 - 6 = -1`, fails the
    /// prevented-damage floor, and declines a block that saves the game — so
    /// reverting the baseline to `attacker_power` fails this test.
    #[test]
    fn double_strike_trample_gang_is_taken_when_it_is_the_difference_between_living_and_dying() {
        let mut state = setup();
        state.players[1].life = 7;
        let attacker = add_creature(
            &mut state,
            PlayerId(0),
            "Attacker",
            5,
            5,
            vec![Keyword::Menace, Keyword::DoubleStrike, Keyword::Trample],
        );
        for i in 0..2 {
            add_creature(
                &mut state,
                PlayerId(1),
                &format!("Blocker {i}"),
                1,
                2,
                vec![],
            );
        }

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert_eq!(
            on_attacker, 2,
            "10 unblocked at 7 life is lethal; the legal pair cuts it to 6 and the \
             player lives. Got {assignments:?}"
        );
    }

    /// A blocker can contribute something absorption cannot express: a first-strike
    /// deathtouch body kills the attacker in the first damage step (CR 702.7b +
    /// CR 702.2c), cancelling its regular-step damage outright.
    ///
    /// 10/10 menace double-strike trampler at 4 life, against two 0/6 walls and a
    /// 1/1 first-strike deathtoucher. By absorption the walls come first, and they
    /// still let 8 through, so the walk then adds the 1/1 — three creatures. But one
    /// wall plus the 1/1 holds it to 3: the attacker takes lethal deathtouch damage
    /// in the first step and never reaches the second. Two creatures, same result.
    ///
    /// Guards the shrink pass; without it the AI sacrifices a blocker it did not need.
    #[test]
    fn survival_gang_drops_blockers_the_rest_of_the_gang_makes_redundant() {
        let mut state = setup();
        state.players[1].life = 4;
        let attacker = add_creature(
            &mut state,
            PlayerId(0),
            "Attacker",
            10,
            10,
            vec![Keyword::Menace, Keyword::DoubleStrike, Keyword::Trample],
        );
        add_creature(&mut state, PlayerId(1), "Wall 1", 0, 6, vec![]);
        add_creature(&mut state, PlayerId(1), "Wall 2", 0, 6, vec![]);
        add_creature(
            &mut state,
            PlayerId(1),
            "Deathtouch Skirmisher",
            1,
            1,
            vec![Keyword::FirstStrike, Keyword::Deathtouch],
        );

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker: Vec<_> = assignments
            .iter()
            .filter(|&&(_, a)| a == attacker)
            .map(|&(b, _)| b)
            .collect();
        assert_eq!(
            on_attacker.len(),
            2,
            "one wall plus the first-strike deathtoucher already holds this to 3 \
             against 4 life — the third blocker is spent for nothing. Got \
             {assignments:?}"
        );
        // Still a legal declaration against the CR 702.111b menace floor.
        assert_eq!(
            engine::game::combat::complete_blocker_proposal(&state, PlayerId(1), &assignments),
            engine::types::actions::GameAction::DeclareBlockers {
                assignments: assignments.clone()
            },
            "the shrunk declaration must still survive the CR 509.1c completion \
             authority"
        );
    }

    /// Ordering: against a trampler a blocker contributes absorption, not cheapness.
    /// One 6/6 absorbs the whole gap that three 1/1s cannot, so the AI must reach for
    /// it rather than walking the value order and spending bodies that do not save.
    ///
    /// 8/8 menace trampler at 3 life: 1/1s absorb 1 each (three of them leave 5), the
    /// 6/6 absorbs 6 and leaves 2. The floor is 2, so the answer is the 6/6 plus one
    /// 1/1 — absorbing 7, leaving 1.
    #[test]
    fn trample_survival_gang_prefers_absorbers_over_cheap_bodies() {
        let mut state = setup();
        state.players[1].life = 3;
        let attacker = add_creature(
            &mut state,
            PlayerId(0),
            "Attacker",
            8,
            8,
            vec![Keyword::Menace, Keyword::Trample],
        );
        let big = add_creature(&mut state, PlayerId(1), "Big", 1, 6, vec![]);
        for i in 0..3 {
            add_creature(&mut state, PlayerId(1), &format!("Small {i}"), 1, 1, vec![]);
        }

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker: Vec<_> = assignments
            .iter()
            .filter(|&&(_, a)| a == attacker)
            .map(|&(b, _)| b)
            .collect();
        assert!(
            on_attacker.contains(&big),
            "CR 702.19b: the 6-toughness blocker absorbs 6 of the 8 — a value-ordered \
             walk spends 1/1s that cannot close the gap. Got {assignments:?}"
        );
        assert_eq!(
            on_attacker.len(),
            2,
            "the floor is 2 and the 6/6 plus one 1/1 already leaves 1 damage — no \
             further creatures should be spent. Got {assignments:?}"
        );
    }

    /// CR 702.19b: "take into account damage already marked on the creature" — a
    /// 4/4 with 3 damage marked needs only 1 more to be lethal, so it absorbs 1, not
    /// 4. Two of them absorb 2 against an 11/11 menace trampler, leaving 9 against
    /// 4 life, and the block must be declined.
    ///
    /// Summing raw toughness reads this board as absorbing 8 and approves a gang
    /// that leaves the player dead — the reason absorption goes through the
    /// resolver's own `lethal_damage_needed` rather than the toughness field.
    #[test]
    fn marked_damage_lowers_absorption_below_the_survival_threshold() {
        let (mut state, attacker, blockers) =
            lethal_attacker_board_with(4, 2, vec![Keyword::Menace, Keyword::Trample]);
        for &bid in &blockers {
            state.objects.get_mut(&bid).unwrap().damage_marked = 3;
        }

        let assignments = choose_blockers(&state, PlayerId(1), &[attacker]);

        let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
        assert_eq!(
            on_attacker, 0,
            "CR 702.19b: damage already marked lowers each blocker's lethal minimum \
             to 1, so the gang absorbs 2 of 11 and the player still dies. Got \
             {assignments:?}"
        );
    }

    /// CR 702.19b: the same boundary without deathtouch, which reaches the
    /// `gang_stabilize` gate rather than exiting at the deathtouch skip. Needs two
    /// attackers to be reachable: `block_is_futile` bounds absorption optimistically
    /// over the whole board, so a lone trampler whose gang cannot absorb is caught
    /// there — but a gang built from what earlier passes left over is not.
    ///
    /// The 6/6 is consumed blocking the 5/5, leaving the menace trampler a
    /// floor-sized gang of two 1/1s. That gang absorbs 2 of 11, so the player takes
    /// 9 at 6 life and dies either way: the block is a pure loss of both creatures.
    #[test]
    fn floored_trampler_is_not_chump_ganged_when_the_gang_cannot_absorb_lethal() {
        let mut state = setup();
        state.players[1].life = 6;
        let plain = add_creature(&mut state, PlayerId(0), "Plain", 5, 5, vec![]);
        let trampler = add_creature(
            &mut state,
            PlayerId(0),
            "Trampler",
            11,
            11,
            vec![Keyword::Menace, Keyword::Trample],
        );
        add_creature(&mut state, PlayerId(1), "Big", 6, 6, vec![]);
        add_creature(&mut state, PlayerId(1), "Small 0", 1, 1, vec![]);
        add_creature(&mut state, PlayerId(1), "Small 1", 1, 1, vec![]);

        let assignments = choose_blockers(&state, PlayerId(1), &[plain, trampler]);

        let on_trampler = assignments.iter().filter(|&&(_, a)| a == trampler).count();
        assert_eq!(
            on_trampler, 0,
            "CR 702.19b: a floor-sized gang absorbing 2 of 11 leaves a lethal residual \
             at 6 life, so the trampler must not be chump-ganged — the creatures are \
             spent and the player dies anyway. Got {assignments:?}"
        );
    }
}
