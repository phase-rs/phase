//! SBA helpers (CR 704).

use engine::game::scenario::GameRunner;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;

use super::AssertionFailure;

/// Trigger an SBA check by passing priority when the engine is waiting for it.
pub fn check_sbas_via_priority(runner: &mut GameRunner) -> Result<(), AssertionFailure> {
    if matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }) {
        return Ok(());
    }
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner
            .act(GameAction::PassPriority)
            .map_err(|e| AssertionFailure {
                kind: "check_sbas".into(),
                detail: format!("PassPriority failed: {e}"),
            })?;
    }
    Ok(())
}
