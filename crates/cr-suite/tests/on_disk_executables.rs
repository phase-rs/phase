//! Discriminating proof: load committed on-disk executable fixtures and run them.

use std::path::PathBuf;

use cr_suite::loader::load_scenarios;
use cr_suite::runner::{run_scenario, RunError};
use cr_suite::schema::ScenarioStatus;

fn scenarios_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

#[test]
fn committed_fixtures_are_executable_seed_only() {
    let loaded = load_scenarios(&scenarios_dir()).expect("load scenarios");
    assert!(
        !loaded.is_empty(),
        "expected committed executable seed fixtures under scenarios/"
    );
    assert!(
        loaded.len() <= 32,
        "seed set too large ({}); skeleton corpus must not be committed — generate in a follow-up PR",
        loaded.len()
    );
    for (path, scenario) in &loaded {
        assert_eq!(
            scenario.status,
            ScenarioStatus::Executable,
            "{} must be status=executable (skeletons belong in a generated follow-up)",
            path.display()
        );
    }
}

#[test]
fn committed_executable_fixtures_pass() {
    let loaded = load_scenarios(&scenarios_dir()).expect("load scenarios");
    let mut failures = Vec::new();

    for (path, scenario) in loaded {
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
        failures.is_empty(),
        "executable fixture failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn claimed_core_rules_are_covered() {
    let loaded = load_scenarios(&scenarios_dir()).expect("load scenarios");
    let rules: std::collections::BTreeSet<_> = loaded.into_iter().map(|(_, s)| s.rule).collect();

    for required in [
        "104.1", "119.1", "120.3", "403.1", "505.1", "704.5a", "704.5f", "704.5g",
    ] {
        assert!(
            rules.contains(required),
            "missing discriminating fixture for CR {required}"
        );
    }
}
