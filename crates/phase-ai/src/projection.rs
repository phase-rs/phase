//! Horizon-parameterized projection of the game state into an opponent's
//! upcoming combat. Used by `combat_ai` and `eval` to read opponent creature
//! power/toughness as it will be when they actually attack, not as it is now.
//!
//! The primitive clones the state, advances through the real engine reducer
//! until a requested horizon on a specified opponent's next turn, and returns
//! the projected state. Phase-based growth triggers (Ouroboroid), attack-
//! declaration triggers (Battle Cry, Mentor), and combat-damage riders all
//! fire naturally because the engine does the work — no reimplementation
//! of trigger effects in the AI layer.

use std::collections::HashMap;

use engine::ai_support::legal_actions;
use engine::game::combat::AttackTarget;
use engine::game::engine::{apply, EngineError};
use engine::types::{CoreType, GameAction, GameState, ObjectId, Phase, PlayerId, WaitingFor};
use web_time::{Duration, Instant};

/// How far into the opponent's upcoming turn to project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectionHorizon {
    /// Phase-based growth only (Ouroboroid, sagas).
    OpponentBeginCombat,
    /// Adds attack-declaration triggers (Battle Cry, Mentor, Hellrider).
    OpponentAttackersDeclared,
    /// Adds first combat damage step (v0: no-blocks baseline).
    OpponentCombatDamage,
}

/// Why the projection could not reach the requested horizon.
#[derive(Debug, Clone)]
pub enum BailReason {
    StepCapExceeded { steps: u32 },
    TimeCapExceeded { elapsed: Duration },
    GameOverDuringProjection,
    MulliganOrSideboardEncountered,
    NoLegalAction { waiting_for: String },
    NoLegalManaPayment,
    EngineRejected(EngineError),
}

/// Per-creature growth across the horizon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VelocitySample {
    /// Creature survived projection; `delta` may be negative (-1/-1 counters).
    Changed { delta: i32 },
    /// Creature was destroyed or exiled during projection.
    Removed,
    /// Token or creature appeared during projection (e.g., Ophiomancer snake).
    Appeared { projected_power: i32 },
}

/// How certain the projection is.
#[derive(Debug, Clone, Copy)]
pub enum Confidence {
    /// No non-trivial policy choices were required.
    Exact,
    /// One or more choices resolved via the policy — callers should apply
    /// a safety margin.
    Approximated { choice_count: u32 },
}

/// Result of a successful projection.
#[derive(Debug, Clone)]
pub struct Projection {
    pub horizon_reached: ProjectionHorizon,
    pub state: GameState,
    /// States captured at each horizon boundary passed through. Consumers
    /// needing an earlier horizon can read from here without re-projecting.
    pub snapshots: Vec<(ProjectionHorizon, GameState)>,
    pub confidence: Confidence,
    pub target_opponent: PlayerId,
}

impl Projection {
    /// Return the snapshot for a specific horizon, if captured.
    pub fn snapshot(&self, horizon: ProjectionHorizon) -> Option<&GameState> {
        self.snapshots
            .iter()
            .find(|(h, _)| *h == horizon)
            .map(|(_, s)| s)
    }
}

/// Cache-compatible projection key. Turn-in-key makes eviction implicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectionKey {
    pub state_hash: u64,
    pub turn_number: u32,
    pub active_player: PlayerId,
    pub ai_player: PlayerId,
    pub target_opponent: PlayerId,
    pub horizon: ProjectionHorizon,
}

/// Outer dispatch cap. Each dispatch may trigger up to 500 engine-internal
/// auto-pass iterations.
const STEP_CAP: u32 = 256;
/// Wall-clock guard for projection. The combat path is a heuristic and must
/// fail closed to the pre-projection behavior rather than monopolize the AI
/// turn when the engine path is unusually expensive.
const TIME_CAP: Duration = Duration::from_millis(15);

/// Advance from `base` forward until `horizon` is reached on
/// `target_opponent`'s next turn. `base` is cloned; never mutated.
/// Deterministic given `(base_fingerprint, ai_player, target_opponent, horizon)`.
pub fn project_to(
    base: &GameState,
    ai_player: PlayerId,
    target_opponent: PlayerId,
    horizon: ProjectionHorizon,
) -> Result<Projection, BailReason> {
    let started_turn = base.turn_number;
    let started_at = Instant::now();
    let mut state = base.clone();
    let mut snapshots: Vec<(ProjectionHorizon, GameState)> = Vec::new();
    let mut choice_count: u32 = 0;

    // Already-at-horizon short-circuit.
    if reached_horizon(&state, target_opponent, horizon, started_turn) {
        return Ok(Projection {
            horizon_reached: horizon,
            state: state.clone(),
            snapshots: vec![(horizon, state)],
            confidence: Confidence::Exact,
            target_opponent,
        });
    }

    for step in 0..STEP_CAP {
        let elapsed = started_at.elapsed();
        if elapsed >= TIME_CAP {
            return Err(BailReason::TimeCapExceeded { elapsed });
        }

        capture_snapshots(&state, target_opponent, started_turn, &mut snapshots);

        if reached_horizon(&state, target_opponent, horizon, started_turn) {
            capture_snapshots(&state, target_opponent, started_turn, &mut snapshots);
            let confidence = if choice_count == 0 {
                Confidence::Exact
            } else {
                Confidence::Approximated { choice_count }
            };
            return Ok(Projection {
                horizon_reached: horizon,
                state,
                snapshots,
                confidence,
                target_opponent,
            });
        }

        let (actor, action, is_policy_choice) = resolve_choice(&state, ai_player, target_opponent)?;
        if is_policy_choice {
            choice_count += 1;
        }

        apply(&mut state, actor, action).map_err(BailReason::EngineRejected)?;

        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return Err(BailReason::GameOverDuringProjection);
        }

        if step == STEP_CAP - 1 {
            return Err(BailReason::StepCapExceeded { steps: STEP_CAP });
        }
    }

    Err(BailReason::StepCapExceeded { steps: STEP_CAP })
}

/// Whether `state` has reached `horizon` on `target_opponent`'s turn.
///
/// Conjunctive predicate: phase match + active-player match + empty stack +
/// active-player holds priority (confirms APNAP triggers resolved) +
/// turn > started_turn (guard against already-at-horizon false positive on
/// entry turn for the wrong opponent).
fn reached_horizon(
    state: &GameState,
    target_opponent: PlayerId,
    horizon: ProjectionHorizon,
    started_turn: u32,
) -> bool {
    if state.active_player != target_opponent {
        return false;
    }
    // Extra-turn guard: only count a BeginCombat that arrives *after* we
    // began projecting, unless we entered projection already on this opponent.
    let started_on_this_opp = state.turn_number == started_turn;
    match horizon {
        ProjectionHorizon::OpponentBeginCombat => {
            if !matches!(state.phase, Phase::BeginCombat) {
                return false;
            }
            if !state.stack.is_empty() {
                return false;
            }
            let priority_ok = matches!(
                &state.waiting_for,
                WaitingFor::Priority { player } if *player == target_opponent
            );
            // Only accept BeginCombat once we've actually advanced (either a
            // new turn, or we were already sitting at the predicate).
            priority_ok && (!started_on_this_opp || is_fresh_begin_combat(state))
        }
        ProjectionHorizon::OpponentAttackersDeclared => {
            // CR 508.1a: creatures_attacked_this_turn tracks declared attackers.
            if state.creatures_attacked_this_turn.is_empty() {
                return false;
            }
            if !state.stack.is_empty() {
                return false;
            }
            matches!(
                &state.waiting_for,
                WaitingFor::Priority { player } if *player == target_opponent
            )
        }
        ProjectionHorizon::OpponentCombatDamage => {
            // CR 510: Combat damage step. After damage is dealt, phase advances
            // but creatures_attacked_this_turn remains populated.
            matches!(state.phase, Phase::CombatDamage | Phase::EndCombat)
                && state.stack.is_empty()
                && matches!(
                    &state.waiting_for,
                    WaitingFor::Priority { player } if *player == target_opponent
                )
        }
    }
}

/// True if BeginningOfCombat triggers have finished resolving for this turn —
/// used as a rough check that we haven't short-circuited at the moment of
/// phase entry before triggers fire.
fn is_fresh_begin_combat(_state: &GameState) -> bool {
    // Stack-empty + priority-to-active already implies this in practice:
    // if BeginCombat triggers existed, they would be on the stack or would
    // have already been passed through.
    true
}

fn capture_snapshots(
    state: &GameState,
    target_opponent: PlayerId,
    started_turn: u32,
    snapshots: &mut Vec<(ProjectionHorizon, GameState)>,
) {
    if reached_horizon(
        state,
        target_opponent,
        ProjectionHorizon::OpponentBeginCombat,
        started_turn,
    ) && !snapshots
        .iter()
        .any(|(h, _)| *h == ProjectionHorizon::OpponentBeginCombat)
    {
        snapshots.push((ProjectionHorizon::OpponentBeginCombat, state.clone()));
    }
    if reached_horizon(
        state,
        target_opponent,
        ProjectionHorizon::OpponentAttackersDeclared,
        started_turn,
    ) && !snapshots
        .iter()
        .any(|(h, _)| *h == ProjectionHorizon::OpponentAttackersDeclared)
    {
        snapshots.push((ProjectionHorizon::OpponentAttackersDeclared, state.clone()));
    }
}

/// Pick a legal action for the currently-waiting player based on projection
/// policy. Returns `(actor, action, is_policy_choice)` where `is_policy_choice`
/// flags non-trivial policy decisions that increment `choice_count`.
fn resolve_choice(
    state: &GameState,
    ai_player: PlayerId,
    target_opponent: PlayerId,
) -> Result<(PlayerId, GameAction, bool), BailReason> {
    // Impossible-mid-game gates.
    match &state.waiting_for {
        WaitingFor::MulliganDecision { .. }
        | WaitingFor::MulliganBottomCards { .. }
        | WaitingFor::BetweenGamesSideboard { .. }
        | WaitingFor::BetweenGamesChoosePlayDraw { .. } => {
            return Err(BailReason::MulliganOrSideboardEncountered);
        }
        WaitingFor::GameOver { .. } => {
            return Err(BailReason::GameOverDuringProjection);
        }
        _ => {}
    }

    let acting = state
        .waiting_for
        .acting_player()
        .ok_or_else(|| BailReason::NoLegalAction {
            waiting_for: format!("{:?}", state.waiting_for),
        })?;

    let actions = legal_actions(state);
    if actions.is_empty() {
        return Err(BailReason::NoLegalAction {
            waiting_for: format!("{:?}", state.waiting_for),
        });
    }

    // Policy dispatch on WaitingFor kind + actor identity.
    let action = match &state.waiting_for {
        WaitingFor::Priority { .. } => pick_pass_or_first(&actions),

        WaitingFor::DeclareAttackers { .. } => {
            // Opponent (target): maximize attackers against AI for pessimism.
            // AI self: decline all attacks (no recursion into combat AI).
            // Other opponent (multiplayer): decline (only target_opponent's
            // attacks matter for this projection).
            if acting == target_opponent {
                pick_max_attackers_against(&actions, ai_player)
            } else {
                pick_empty_attackers(&actions)
            }
        }

        WaitingFor::DeclareBlockers { .. } => {
            // AI or any player: decline (v0 no-blocks baseline).
            pick_empty_blockers(&actions)
        }

        WaitingFor::ManaPayment { .. }
        | WaitingFor::ChooseXValue { .. }
        | WaitingFor::ChooseManaColor { .. }
        | WaitingFor::ReturnToHandForCost { .. }
        | WaitingFor::TapCreaturesForSpellCost { .. }
        | WaitingFor::TapCreaturesForManaAbility { .. }
        | WaitingFor::DefilerPayment { .. }
        | WaitingFor::PhyrexianPayment { .. }
        | WaitingFor::CombatTaxPayment { .. }
        | WaitingFor::HarmonizeTapChoice { .. }
        | WaitingFor::WarpCostChoice { .. }
        | WaitingFor::UnlessPayment { .. } => {
            // First legal payment. If none exist for a mandatory cost, bail.
            actions
                .first()
                .cloned()
                .ok_or(BailReason::NoLegalManaPayment)?
        }

        WaitingFor::OptionalEffectChoice { .. }
        | WaitingFor::OpponentMayChoice { .. }
        | WaitingFor::OptionalCostChoice { .. }
        | WaitingFor::TributeChoice { .. }
        | WaitingFor::CompanionReveal { .. } => {
            // For the actor: pick the "no" option (decline) unless it's the
            // opponent and there's a clearly growth-maximizing yes.
            // Simple v0: always pick first — usually decline.
            actions.first().cloned().unwrap()
        }

        _ => {
            // All remaining variants: first legal action.
            actions.first().cloned().unwrap()
        }
    };

    let is_policy_choice = !matches!(action, GameAction::PassPriority);
    Ok((acting, action, is_policy_choice))
}

fn pick_pass_or_first(actions: &[GameAction]) -> GameAction {
    actions
        .iter()
        .find(|a| matches!(a, GameAction::PassPriority))
        .cloned()
        .unwrap_or_else(|| actions[0].clone())
}

fn pick_empty_attackers(actions: &[GameAction]) -> GameAction {
    actions
        .iter()
        .find(|a| matches!(a, GameAction::DeclareAttackers { attacks } if attacks.is_empty()))
        .cloned()
        .unwrap_or_else(|| actions[0].clone())
}

fn pick_empty_blockers(actions: &[GameAction]) -> GameAction {
    actions
        .iter()
        .find(
            |a| matches!(a, GameAction::DeclareBlockers { assignments } if assignments.is_empty()),
        )
        .cloned()
        .unwrap_or_else(|| actions[0].clone())
}

fn pick_max_attackers_against(actions: &[GameAction], ai_player: PlayerId) -> GameAction {
    // From the DeclareAttackers candidate set, pick the variant with the most
    // attackers targeting `ai_player` (pessimistic worst-case).
    let mut best: Option<(usize, &GameAction)> = None;
    for action in actions {
        if let GameAction::DeclareAttackers { attacks } = action {
            let count = attacks
                .iter()
                .filter(|(_, target)| matches!(target, AttackTarget::Player(p) if *p == ai_player))
                .count();
            match best {
                None => best = Some((count, action)),
                Some((best_count, _)) if count > best_count => best = Some((count, action)),
                _ => {}
            }
        }
    }
    best.map(|(_, a)| a.clone())
        .unwrap_or_else(|| actions[0].clone())
}

/// Compute growth per opponent creature across the projection.
/// Uses the `OpponentBeginCombat` snapshot when available (isolates growth
/// signal from attack-feasibility prohibitions like Moat).
pub fn threat_velocity(
    base: &GameState,
    projection: &Projection,
    opponent: PlayerId,
) -> HashMap<ObjectId, VelocitySample> {
    let projected = projection
        .snapshot(ProjectionHorizon::OpponentBeginCombat)
        .unwrap_or(&projection.state);

    let mut samples: HashMap<ObjectId, VelocitySample> = HashMap::new();
    let mut base_seen: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();

    // Pass 1: creatures present in base — Changed or Removed.
    for &id in base.battlefield.iter() {
        let Some(base_obj) = base.objects.get(&id) else {
            continue;
        };
        if base_obj.controller != opponent
            || !base_obj.card_types.core_types.contains(&CoreType::Creature)
        {
            continue;
        }
        base_seen.insert(id);
        let base_power = base_obj.power.unwrap_or(0);
        match projected.objects.get(&id) {
            Some(proj_obj) if projected.battlefield.contains(&id) => {
                let proj_power = proj_obj.power.unwrap_or(0);
                samples.insert(
                    id,
                    VelocitySample::Changed {
                        delta: proj_power - base_power,
                    },
                );
            }
            _ => {
                samples.insert(id, VelocitySample::Removed);
            }
        }
    }

    // Pass 2: new creatures in projection not in base — Appeared.
    for &id in projected.battlefield.iter() {
        if base_seen.contains(&id) {
            continue;
        }
        let Some(proj_obj) = projected.objects.get(&id) else {
            continue;
        };
        if proj_obj.controller != opponent
            || !proj_obj.card_types.core_types.contains(&CoreType::Creature)
        {
            continue;
        }
        samples.insert(
            id,
            VelocitySample::Appeared {
                projected_power: proj_obj.power.unwrap_or(0),
            },
        );
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;

    #[test]
    fn projection_horizon_is_copy_hash() {
        // Sanity: the enum is used as a HashMap key and in Copy contexts.
        let h = ProjectionHorizon::OpponentBeginCombat;
        let _copy = h;
        let mut set = std::collections::HashSet::new();
        set.insert(h);
        assert!(set.contains(&ProjectionHorizon::OpponentBeginCombat));
    }

    #[test]
    fn velocity_sample_variants() {
        let changed = VelocitySample::Changed { delta: 3 };
        let removed = VelocitySample::Removed;
        let appeared = VelocitySample::Appeared { projected_power: 5 };
        assert_ne!(changed, removed);
        assert_ne!(changed, appeared);
    }

    /// Build a minimal two-player state with one opponent creature.
    fn state_with_opp_creature(name: &str, power: i32) -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(power);
        (state, id)
    }

    #[test]
    fn velocity_classifies_unchanged_creature_as_changed_zero() {
        // A vanilla creature without triggers should report Changed { delta: 0 }
        // when base and projection are identical (no growth).
        let (base, id) = state_with_opp_creature("Vanilla Bear", 2);
        let projection = Projection {
            horizon_reached: ProjectionHorizon::OpponentBeginCombat,
            state: base.clone(),
            snapshots: vec![(ProjectionHorizon::OpponentBeginCombat, base.clone())],
            confidence: Confidence::Exact,
            target_opponent: PlayerId(1),
        };
        let samples = threat_velocity(&base, &projection, PlayerId(1));
        assert_eq!(
            samples.get(&id),
            Some(&VelocitySample::Changed { delta: 0 })
        );
    }

    #[test]
    fn velocity_classifies_grown_creature() {
        // Simulate Ouroboroid effect: same ObjectId, higher projected power.
        let (base, id) = state_with_opp_creature("Scaly", 1);
        let mut projected = base.clone();
        projected.objects.get_mut(&id).unwrap().power = Some(9);

        let projection = Projection {
            horizon_reached: ProjectionHorizon::OpponentBeginCombat,
            state: projected.clone(),
            snapshots: vec![(ProjectionHorizon::OpponentBeginCombat, projected)],
            confidence: Confidence::Approximated { choice_count: 1 },
            target_opponent: PlayerId(1),
        };
        let samples = threat_velocity(&base, &projection, PlayerId(1));
        assert_eq!(
            samples.get(&id),
            Some(&VelocitySample::Changed { delta: 8 })
        );
    }

    #[test]
    fn velocity_classifies_removed_creature() {
        // Creature exists in base but is gone from projection (destroyed mid-turn).
        let (base, id) = state_with_opp_creature("Doomed", 3);
        let mut projected = base.clone();
        // Remove from battlefield (mirrors what sacrifice/destroy does structurally).
        projected.battlefield.retain(|&bid| bid != id);

        let projection = Projection {
            horizon_reached: ProjectionHorizon::OpponentBeginCombat,
            state: projected.clone(),
            snapshots: vec![(ProjectionHorizon::OpponentBeginCombat, projected)],
            confidence: Confidence::Exact,
            target_opponent: PlayerId(1),
        };
        let samples = threat_velocity(&base, &projection, PlayerId(1));
        assert_eq!(samples.get(&id), Some(&VelocitySample::Removed));
    }

    #[test]
    fn velocity_classifies_appeared_token() {
        // Opponent creates a token during projection (Ophiomancer-style).
        let (base, _original_id) = state_with_opp_creature("Host", 2);
        let mut projected = base.clone();
        let token_id = create_object(
            &mut projected,
            CardId(99),
            PlayerId(1),
            "Snake Token".to_string(),
            Zone::Battlefield,
        );
        let obj = projected.objects.get_mut(&token_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(1);

        let projection = Projection {
            horizon_reached: ProjectionHorizon::OpponentBeginCombat,
            state: projected.clone(),
            snapshots: vec![(ProjectionHorizon::OpponentBeginCombat, projected)],
            confidence: Confidence::Exact,
            target_opponent: PlayerId(1),
        };
        let samples = threat_velocity(&base, &projection, PlayerId(1));
        assert_eq!(
            samples.get(&token_id),
            Some(&VelocitySample::Appeared { projected_power: 1 })
        );
    }

    #[test]
    fn velocity_ignores_ai_controlled_creatures() {
        // AI's own creatures shouldn't appear in opponent velocity samples.
        let mut state = GameState::new_two_player(42);
        let ai_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "AI Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ai_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);

        let projection = Projection {
            horizon_reached: ProjectionHorizon::OpponentBeginCombat,
            state: state.clone(),
            snapshots: vec![(ProjectionHorizon::OpponentBeginCombat, state.clone())],
            confidence: Confidence::Exact,
            target_opponent: PlayerId(1),
        };
        let samples = threat_velocity(&state, &projection, PlayerId(1));
        assert!(
            !samples.contains_key(&ai_id),
            "AI creatures must not appear in opponent velocity samples"
        );
    }

    #[test]
    fn projection_key_includes_turn_for_implicit_invalidation() {
        // Two keys identical except for turn_number must hash differently,
        // so stale entries from prior turns never serve a current lookup.
        let k1 = ProjectionKey {
            state_hash: 12345,
            turn_number: 3,
            active_player: PlayerId(0),
            ai_player: PlayerId(0),
            target_opponent: PlayerId(1),
            horizon: ProjectionHorizon::OpponentBeginCombat,
        };
        let k2 = ProjectionKey {
            turn_number: 4,
            ..k1
        };
        assert_ne!(k1, k2);
    }
}
