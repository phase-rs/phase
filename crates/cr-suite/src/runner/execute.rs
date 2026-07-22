//! Top-level scenario execution.

use crate::assert::evaluate_assertion;
use crate::runner::setup::build_runner;
use crate::runner::steps::apply_step;
use crate::runner::RunError;
use crate::schema::{ScenarioFile, ScenarioStatus};

/// Execute one scenario. Skeletons / non-executable statuses return Ok without running.
pub fn run_scenario(scenario: &ScenarioFile) -> Result<(), RunError> {
    if scenario.status != ScenarioStatus::Executable {
        return Ok(());
    }

    let setup = scenario
        .setup
        .as_ref()
        .ok_or_else(|| RunError::Setup("executable scenario missing [setup]".into()))?;

    let (mut runner, mut ctx) = build_runner(setup)?;

    for (idx, step) in scenario.steps.iter().enumerate() {
        apply_step(&mut runner, &mut ctx, step).map_err(|e| match e {
            RunError::Step(msg) => RunError::Step(format!("step {idx}: {msg}")),
            other => other,
        })?;
    }

    for (idx, assertion) in scenario.assertions.iter().enumerate() {
        evaluate_assertion(&runner, &ctx.handles, assertion).map_err(|failure| {
            RunError::Assertion(crate::assert::AssertionFailure {
                kind: failure.kind,
                detail: format!("assertion {idx}: {}", failure.detail),
            })
        })?;
    }

    Ok(())
}
