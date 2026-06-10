// Integration test entry point for rules correctness tests.
// Common imports re-exported for all rule test modules via `use super::*`.
#![allow(unused_imports)]

pub use engine::game::apply;
pub use engine::game::combat::AttackTarget;
pub use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
pub use engine::types::actions::GameAction;
pub use engine::types::events::GameEvent;
pub use engine::types::game_state::{
    ActionResult, CastPaymentMode, CostResume, DamageSlot, PayCostKind, WaitingFor,
};
pub use engine::types::identifiers::ObjectId;
pub use engine::types::keywords::Keyword;
pub use engine::types::phase::Phase;
pub use engine::types::player::PlayerId;
pub use engine::types::zones::{ExileCostSourceZone, Zone};

/// Shared combat helper: drives the engine from DeclareAttackers through damage resolution.
///
/// Assumes the runner is at a phase where passing priority twice will reach DeclareAttackers
/// (i.e., the scenario started at `Phase::PreCombatMain`). All attackers target P1.
pub fn run_combat(
    runner: &mut GameRunner,
    attacker_ids: Vec<ObjectId>,
    blocker_assignments: Vec<(ObjectId, ObjectId)>,
) {
    run_combat_with_blocker_divisions(runner, attacker_ids, blocker_assignments, &[]);
}

/// Banding-aware variant of [`run_combat`] (CR 702.22k): drives the same
/// DeclareAttackers → damage path but also resolves the interactive
/// `WaitingFor::AssignBlockerDamage` prompt the engine raises when a blocker is
/// blocking a banding attacker (the active player divides that blocker's damage
/// among the attackers it blocks).
///
/// `blocker_divisions` maps a `blocker_id` to the `(attacker_id, damage)` split
/// the test wants submitted for that blocker. Any blocker not listed (or when an
/// `AssignBlockerDamage` prompt names attackers none of the divisions cover) is
/// resolved with an even auto-split that sums to the blocker's power, so callers
/// that don't care about a specific division still resolve cleanly.
pub fn run_combat_with_blocker_divisions(
    runner: &mut GameRunner,
    attacker_ids: Vec<ObjectId>,
    blocker_assignments: Vec<(ObjectId, ObjectId)>,
    blocker_divisions: &[(ObjectId, Vec<(ObjectId, u32)>)],
) {
    runner.pass_both_players();

    let attacks: Vec<_> = attacker_ids
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();

    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // CR 508.2: Active player gets priority after attackers — pass through it.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }

    // CR 509.1: Interactive blocker declaration only when the defender has legal
    // blockers. When none exist, the engine auto-submits empty blockers internally
    // (CR 509.1 + CR 117.1c — the step still runs and AP still gets priority).
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: blocker_assignments,
            })
            .expect("DeclareBlockers should succeed");
    }

    // CR 509.2 + CR 117.1c: Active player receives priority during the declare
    // blockers step — always, even when no blockers were declared. Pass through.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }

    // CR 510.1c / CR 510.1d + CR 702.22j/k: Handle interactive damage assignment.
    // The engine raises `AssignCombatDamage` for an attacker dividing its damage
    // among multiple blockers, and `AssignBlockerDamage` for a blocker dividing
    // its damage among multiple banded attackers. Both can appear (and re-appear
    // across the first-strike/regular sub-steps), so loop until neither remains.
    loop {
        match &runner.state().waiting_for {
            WaitingFor::AssignCombatDamage {
                blockers,
                total_damage,
                trample,
                ..
            } => {
                let mut remaining = *total_damage;
                let mut assignments: Vec<(ObjectId, u32)> = Vec::new();
                for slot in blockers {
                    let assign = remaining.min(slot.lethal_minimum);
                    assignments.push((slot.blocker_id, assign));
                    remaining = remaining.saturating_sub(assign);
                }
                // Non-trample: dump remainder to last blocker so total == power.
                if trample.is_none() && remaining > 0 {
                    if let Some(last) = assignments.last_mut() {
                        last.1 += remaining;
                        remaining = 0;
                    }
                }
                let trample_damage = if trample.is_some() { remaining } else { 0 };
                runner
                    .act(GameAction::AssignCombatDamage {
                        mode: engine::types::game_state::CombatDamageAssignmentMode::Normal,
                        assignments,
                        trample_damage,
                        controller_damage: 0,
                    })
                    .expect("AssignCombatDamage should succeed");
            }
            WaitingFor::AssignBlockerDamage {
                blocker_id,
                total_damage,
                attackers,
                ..
            } => {
                // Use a caller-provided division for this blocker if present,
                // else fall back to an even auto-split (first attacker gets the
                // remainder) so the total equals the blocker's power (CR 510.1e).
                let assignments: Vec<(ObjectId, u32)> = blocker_divisions
                    .iter()
                    .find(|(bid, _)| bid == blocker_id)
                    .map(|(_, div)| div.clone())
                    .unwrap_or_else(|| even_split(*total_damage, attackers));
                runner
                    .act(GameAction::AssignBlockerDamage { assignments })
                    .expect("AssignBlockerDamage should succeed");
            }
            _ => break,
        }
    }
}

/// Evenly divide `total` damage among `targets` (first target absorbs the
/// remainder) so the assignment sums to `total` (CR 510.1e). Used as the
/// harness's default when a test doesn't dictate a specific banded division.
fn even_split(total: u32, targets: &[ObjectId]) -> Vec<(ObjectId, u32)> {
    if targets.is_empty() {
        return Vec::new();
    }
    let n = targets.len() as u32;
    let base = total / n;
    let remainder = total % n;
    targets
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, base + if (i as u32) < remainder { 1 } else { 0 }))
        .collect()
}

// Mechanic test modules (stubs -- populated in Plans 02 and 03)
mod attractions;
mod battle;
#[path = "rules/casting.rs"]
mod casting;
#[path = "rules/combat.rs"]
mod combat;
#[path = "rules/etb.rs"]
mod etb;
#[path = "rules/keywords.rs"]
mod keywords;
#[path = "rules/layers.rs"]
mod layers;
#[path = "rules/replacement.rs"]
mod replacement;
#[path = "rules/sba.rs"]
mod sba;
#[path = "rules/stack.rs"]
mod stack;
#[path = "rules/targeting.rs"]
mod targeting;
#[path = "rules/tribute.rs"]
mod tribute;
