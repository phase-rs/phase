//! Guard: no test files directly under `crates/engine/tests/`.
//!
//! Cargo autotest discovery compiles every `tests/*.rs` file into its own
//! test binary, and each such binary statically links the full engine
//! (~130MB debug link). At one point 206 files had accumulated there,
//! adding ~30 minutes of link time to every engine change. All integration
//! tests must instead live in `tests/integration/` and be registered with a
//! `mod` line in `tests/integration/main.rs`, which compiles them into this
//! single harness binary. nextest still runs every test in its own process,
//! so binary-level separation adds no isolation.

use std::collections::HashSet;
use std::path::Path;

#[test]
fn tests_dir_has_no_top_level_rs_files() {
    let tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders: Vec<String> = std::fs::read_dir(&tests_dir)
        .expect("read crates/engine/tests/")
        .filter_map(|entry| {
            let entry = entry.expect("read dir entry");
            let path = entry.path();
            (path.is_file() && path.extension().is_some_and(|ext| ext == "rs"))
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
        .collect();
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "Found top-level test file(s) in crates/engine/tests/: {offenders:?}.\n\
         Each becomes its own ~130MB test binary linking the full engine. \
         Move the file into crates/engine/tests/integration/ and add a \
         `mod` line to tests/integration/main.rs instead."
    );
}

/// The converse guard: manual `mod` registration (unlike cargo autotest
/// discovery) can silently forget a file — an unregistered `.rs` file in
/// `tests/integration/` compiles into nothing and its tests never run while
/// the suite stays green. (The other direction, a `mod` line without a file,
/// is already a compile error.)
#[test]
fn every_integration_file_is_registered_in_main_rs() {
    let integration_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("integration");
    let registered: HashSet<String> = std::fs::read_to_string(integration_dir.join("main.rs"))
        .expect("read tests/integration/main.rs")
        .lines()
        .filter_map(|line| {
            Some(
                line.trim()
                    .strip_prefix("mod ")?
                    .trim_end_matches(';')
                    .into(),
            )
        })
        .collect();
    let mut unregistered: Vec<String> = std::fs::read_dir(&integration_dir)
        .expect("read crates/engine/tests/integration/")
        .filter_map(|entry| {
            let entry = entry.expect("read dir entry");
            let path = entry.path();
            let stem = path.file_stem()?.to_string_lossy().into_owned();
            (path.is_file()
                && path.extension().is_some_and(|ext| ext == "rs")
                && stem != "main"
                && !registered.contains(&stem))
            .then_some(stem)
        })
        .collect();
    unregistered.sort();
    assert!(
        unregistered.is_empty(),
        "Test file(s) in crates/engine/tests/integration/ missing a `mod` line \
         in tests/integration/main.rs: {unregistered:?}. Unregistered files are \
         silently never compiled or run."
    );
}
