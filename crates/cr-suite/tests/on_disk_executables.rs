//! Discriminating proof: load committed on-disk fixtures and run executables.

use std::path::PathBuf;

use cr_suite::loader::load_scenarios;
use cr_suite::runner::{run_scenario, RunError};
use cr_suite::schema::ScenarioStatus;

fn scenarios_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

#[test]
fn committed_seed_is_small_and_status_typed() {
    let loaded = load_scenarios(&scenarios_dir()).expect("load scenarios");
    assert!(
        !loaded.is_empty(),
        "expected committed seed fixtures under scenarios/"
    );
    assert!(
        loaded.len() <= 32,
        "seed set too large ({}); skeleton corpus must not be committed",
        loaded.len()
    );
    for (path, scenario) in &loaded {
        assert!(
            matches!(
                scenario.status,
                ScenarioStatus::Executable | ScenarioStatus::Deferred
            ),
            "{} has unexpected status {:?}",
            path.display(),
            scenario.status
        );
    }
}

#[test]
fn committed_executable_fixtures_pass() {
    let loaded = load_scenarios(&scenarios_dir()).expect("load scenarios");
    let mut ran = 0usize;
    let mut failures = Vec::new();

    for (path, scenario) in loaded {
        if scenario.status != ScenarioStatus::Executable {
            continue;
        }
        ran += 1;
        match run_scenario(&scenario) {
            Ok(()) => {}
            Err(RunError::Assertion(failure)) => {
                failures.push(format!(
                    "{} ({}): assertion {} — {}",
                    scenario.rule,
                    path.display(),
                    failure.kind,
                    failure.detail
                ));
            }
            Err(RunError::Setup(msg) | RunError::Step(msg)) => {
                failures.push(format!("{} ({}): {}", scenario.rule, path.display(), msg));
            }
        }
    }

    assert!(
        ran >= 4,
        "expected at least 4 executable fixtures, got {ran}"
    );
    assert!(
        failures.is_empty(),
        "executable fixture failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn discriminating_executable_rules_are_present() {
    let loaded = load_scenarios(&scenarios_dir()).expect("load scenarios");
    let executable: std::collections::BTreeSet<_> = loaded
        .into_iter()
        .filter(|(_, s)| s.status == ScenarioStatus::Executable)
        .map(|(_, s)| s.rule)
        .collect();

    for required in ["104.1", "704.5a", "704.5f", "704.5g"] {
        assert!(
            executable.contains(required),
            "missing discriminating executable fixture for CR {required}; have {executable:?}"
        );
    }
}

#[test]
fn setup_only_rules_are_not_marked_executable() {
    let loaded = load_scenarios(&scenarios_dir()).expect("load scenarios");
    for rule in ["119.1", "120.3", "403.1", "505.1"] {
        let status = loaded
            .iter()
            .find(|(_, s)| s.rule == rule)
            .map(|(_, s)| s.status);
        assert_ne!(
            status,
            Some(ScenarioStatus::Executable),
            "CR {rule} must not be executable until it drives a production transition"
        );
    }
}
