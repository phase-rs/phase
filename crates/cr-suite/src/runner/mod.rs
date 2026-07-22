//! Execute declarative CR scenarios against the engine.

mod execute;
mod setup;
mod steps;

use std::path::{Path, PathBuf};

use crate::assert::{AssertionFailure, HandleMap};
use crate::loader::load_scenarios;
use crate::report::{ScenarioReport, SuiteReport};
use crate::schema::ScenarioStatus;

pub use execute::run_scenario;
pub use setup::build_runner;
pub use steps::apply_step;

/// Filter controlling which fixtures the suite runner considers.
#[derive(Debug, Clone, Default)]
pub struct RunFilter {
    pub sections: Option<Vec<u32>>,
    pub rules: Option<Vec<String>>,
    /// When true, also attempt skeletons (they will skip).
    pub include_non_executable: bool,
}

/// Suite run options.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub scenarios_dir: PathBuf,
    pub filter: RunFilter,
    pub fail_fast: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioOutcome {
    Passed,
    Failed(AssertionFailure),
    Skipped { reason: String },
    Error { message: String },
}

/// Run all matching fixtures under `opts.scenarios_dir`.
pub fn run_suite(opts: &RunOptions) -> Result<SuiteReport, String> {
    let loaded = load_scenarios(&opts.scenarios_dir).map_err(|e| e.to_string())?;
    let mut report = SuiteReport::default();

    for (path, scenario) in loaded {
        if let Some(sections) = &opts.filter.sections {
            if !sections.contains(&scenario.section) {
                continue;
            }
        }
        if let Some(rules) = &opts.filter.rules {
            if !rules.iter().any(|r| r == &scenario.rule) {
                continue;
            }
        }

        if scenario.status != ScenarioStatus::Executable && !opts.filter.include_non_executable {
            report.skipped += 1;
            report.results.push(ScenarioReport {
                path: path.clone(),
                rule: scenario.rule.clone(),
                section: scenario.section,
                outcome: ScenarioOutcome::Skipped {
                    reason: format!("status={:?}", scenario.status),
                },
            });
            continue;
        }

        let outcome = match scenario.status {
            ScenarioStatus::Executable => match run_scenario(&scenario) {
                Ok(()) => ScenarioOutcome::Passed,
                Err(RunError::Assertion(failure)) => ScenarioOutcome::Failed(failure),
                Err(RunError::Setup(message) | RunError::Step(message)) => {
                    ScenarioOutcome::Error { message }
                }
            },
            ScenarioStatus::Skeleton => ScenarioOutcome::Skipped {
                reason: "skeleton".into(),
            },
            ScenarioStatus::NotApplicable => ScenarioOutcome::Skipped {
                reason: "not-applicable".into(),
            },
            ScenarioStatus::Deferred => ScenarioOutcome::Skipped {
                reason: "deferred".into(),
            },
        };

        match &outcome {
            ScenarioOutcome::Passed => report.passed += 1,
            ScenarioOutcome::Failed(_) => report.failed += 1,
            ScenarioOutcome::Skipped { .. } => report.skipped += 1,
            ScenarioOutcome::Error { .. } => report.errors += 1,
        }

        let is_failure = matches!(
            outcome,
            ScenarioOutcome::Failed(_) | ScenarioOutcome::Error { .. }
        );

        report.results.push(ScenarioReport {
            path,
            rule: scenario.rule,
            section: scenario.section,
            outcome,
        });

        if opts.fail_fast && is_failure {
            break;
        }
    }

    Ok(report)
}

#[derive(Debug)]
pub enum RunError {
    Setup(String),
    Step(String),
    Assertion(AssertionFailure),
}

/// Convenience: run a single fixture file path.
pub fn run_scenario_file(path: &Path) -> Result<(), RunError> {
    let scenario =
        crate::loader::load_scenario_file(path).map_err(|e| RunError::Setup(e.to_string()))?;
    run_scenario(&scenario)
}

/// Shared context while a scenario executes.
pub struct ScenarioContext {
    pub handles: HandleMap,
}
