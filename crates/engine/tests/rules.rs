// Integration test entry point for rules correctness tests.
// Common imports re-exported for all rule test modules via `use super::*`.
#![allow(unused_imports)]

pub use engine::game::apply;
pub use engine::game::combat::AttackTarget;
pub use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
pub use engine::types::actions::GameAction;
pub use engine::types::events::GameEvent;
pub use engine::types::game_state::{ActionResult, WaitingFor};
pub use engine::types::identifiers::ObjectId;
pub use engine::types::keywords::Keyword;
pub use engine::types::phase::Phase;
pub use engine::types::player::PlayerId;
pub use engine::types::zones::Zone;

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
}

// Mechanic test modules (stubs -- populated in Plans 02 and 03)
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
