// Integration test entry point for rules correctness tests.
// Common imports re-exported for all rule test modules via `use super::*`.
#![allow(unused_imports)]

pub use engine::game::apply;
pub use engine::game::combat::AttackTarget;
pub use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
pub use engine::types::actions::GameAction;
pub use engine::types::events::GameEvent;
pub use engine::types::game_state::{
    ActionResult, CostResume, DamageSlot, PayCostKind, WaitingFor,
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
    runner.pass_both_players();

    let attacks: Vec<_> = attacker_ids
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();

    runner
        .act(GameAction::DeclareAttackers { attacks })
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

    // CR 510.1c: Handle interactive damage assignment for 2+ blocker scenarios.
    while let WaitingFor::AssignCombatDamage {
        blockers,
        total_damage,
        trample,
        ..
    } = &runner.state().waiting_for
    {
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
}

// Mechanic test modules (stubs -- populated in Plans 02 and 03)
#[path = "rules/battle.rs"]
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
