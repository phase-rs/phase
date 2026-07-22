//! Stack assertions (CR 405 / CR 608).

use engine::game::scenario::GameRunner;

use super::AssertionFailure;

pub fn stack_is_empty(runner: &GameRunner) -> Result<(), AssertionFailure> {
    if !runner.state().stack.is_empty() {
        return Err(AssertionFailure {
            kind: "stack_empty".into(),
            detail: format!("stack has {} entries", runner.state().stack.len()),
        });
    }
    Ok(())
}
