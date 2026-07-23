//! Discriminating proof: load committed on-disk fixtures and run executables.

use std::path::PathBuf;

use cr_suite::loader::load_scenarios;
use cr_suite::runner::{run_scenario, RunError};
use cr_suite::schema::ScenarioStatus;

fn scenarios_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

#[test]
fn authored_seed_is_typed_and_bounded() {
    // The generated skeleton corpus (thousands of `status = "skeleton"` files)
    // may coexist under scenarios/ alongside the hand-authored seed set. This
    // test only constrains the *authored* fixtures (Executable / Deferred):
    // they must stay a small, reviewable set. Skeletons are unbounded and are
    // not required to carry setup/steps/assertions.
    let loaded = load_scenarios(&scenarios_dir()).expect("load scenarios");
    assert!(
        !loaded.is_empty(),
        "expected committed fixtures under scenarios/"
    );

    let authored: Vec<_> = loaded
        .iter()
        .filter(|(_, s)| {
            matches!(
                s.status,
                ScenarioStatus::Executable | ScenarioStatus::Deferred
            )
        })
        .collect();

    assert!(
        !authored.is_empty(),
        "expected at least some authored (executable/deferred) fixtures"
    );
    assert!(
        authored.len() <= 256,
        "authored seed set too large ({}); did skeletons leak in as authored?",
        authored.len()
    );

    // Every fixture that *claims* to be executable must be well-formed enough to
    // run (have a setup). Skeletons and not-applicable entries are exempt.
    for (path, scenario) in &loaded {
        if scenario.status == ScenarioStatus::Executable {
            assert!(
                scenario.setup.is_some(),
                "{} claims executable but has no [setup]",
                path.display()
            );
        }
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
