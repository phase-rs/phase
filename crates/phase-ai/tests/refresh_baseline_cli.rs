//! The baseline's file-state contract, asserted against the real binary.
//!
//! Review found that the refusal this crate's `--refresh-baseline` guard adds could fire *after*
//! the damage it exists to prevent: `run_suite` writes its report to the suite's output path
//! before any guard runs, so pointing `--current-output` at the baseline truncated the baseline
//! and only then printed "refusing to refresh". Measured on the pre-fix binary: 116 bytes in,
//! 250 out, different sha256, exit 1. A guard whose subject is already destroyed is not a guard.
//!
//! Every assertion here is on the pair (exit status, baseline bytes), because either alone is
//! satisfied by a broken implementation: exiting non-zero while having clobbered the file is the
//! bug itself, and leaving the file alone while exiting 0 would bless the run.
//!
//! These run the real CLI. The aliasing tests are rejected at the argument check and never reach
//! the card database at all. The ones that must run the suite to reach what they test supply
//! their own database — the literal `{}`, which `CardDatabase::from_export` accepts as an empty
//! map because it deserialises a map of name→entry. Nothing here loads the ~90 MB export, so the
//! file stays in the millisecond range and adds no per-test parse.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Not a valid suite report, and that is deliberate: a test using this as the baseline must fail
/// if the binary ever parses it, because parsing it means it got further than the check under
/// test. Usable only where the refusal precedes the baseline read — since the malformed-baseline
/// fix, a refresh that reaches `load_report` refuses on this rather than running the suite.
const SENTINEL: &str = r#"{"this":"is the trusted baseline, byte for byte"}"#;

/// Marks a pre-seeded `--current-output`. On the refresh path the refreshed baseline IS the run's
/// report, so this file must come back untouched whether the run is refused or accepted.
const CURRENT_MARKER: &str = "CURRENT-OUTPUT-MARKER";

/// A scratch dir per `tag`. Every caller passes a tag no other caller uses, so the pre-clean
/// cannot race a sibling test in this binary.
///
/// Pre-cleaned because pids are reused and a failed run leaves its dir behind; without it the
/// next run dies at `create_new`, `symlink()` or `hard_link()` with EEXIST — an infrastructure
/// failure wearing the costume of an assertion failure.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phase-refresh-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn seed_baseline(dir: &Path) -> PathBuf {
    let path = dir.join("suite-baseline.json");
    std::fs::write(&path, SENTINEL).expect("seed baseline");
    path
}

/// A valid, empty card database: the export deserialises a map from card name to entry, so `{}`
/// is a legal database with no cards. Returns the `--data-root` argument.
fn empty_card_db(dir: &Path) -> String {
    let root = dir.join("cards");
    std::fs::create_dir_all(&root).expect("create data root");
    std::fs::write(root.join("card-data.json"), "{}").expect("write card data");
    root.display().to_string()
}

/// Record a real, parseable baseline by running the binary once.
///
/// Tests whose subject lies PAST the baseline read can no longer use `SENTINEL`: a refresh now
/// refuses an existing baseline it cannot parse, so an invalid fixture would stop the run before
/// it reached the thing under test — and would do so while still satisfying a naive
/// "bytes unchanged" assertion. Recording a real one costs about two milliseconds, because an
/// empty card database still yields one degenerate but playable matchup.
fn record_baseline(dir: &Path, data_arg: &str, name: &str) -> PathBuf {
    let path = dir.join(name);
    let (code, stderr) = run(&[
        "--refresh-baseline",
        "--data-root",
        data_arg,
        "--baseline",
        &path.display().to_string(),
        "--current-output",
        &dir.join("record-current.json").display().to_string(),
        "--suite-filter",
        "red-mirror",
        "--games",
        "1",
    ]);
    assert_eq!(
        code,
        Some(0),
        "recording a baseline failed; stderr:\n{stderr}"
    );
    path
}

fn run(args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_ai-gate"))
        .args(args)
        .output()
        .expect("spawn ai-gate");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The recorded defect, in the direction that destroyed data: a refresh whose output path is the
/// baseline must be refused with the baseline still byte-identical.
#[test]
fn an_aliased_refresh_is_refused_before_the_baseline_can_be_overwritten() {
    let dir = scratch("refresh");
    let baseline = seed_baseline(&dir);
    let path = baseline.display().to_string();

    let (code, stderr) = run(&[
        "--refresh-baseline",
        "--baseline",
        &path,
        "--current-output",
        &path,
        "--suite-filter",
        "no-such-matchup",
    ]);

    assert_ne!(
        code,
        Some(0),
        "an aliased refresh must fail; stderr:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&baseline).expect("read baseline"),
        SENTINEL,
        "the baseline was modified by a run that was refused"
    );
    // LOAD-BEARING — see the module note. Measured: with the alias check deleted, or with
    // `same_file` forced to false, the two assertions above still pass, because the binary then
    // fails at the card database with the baseline equally untouched. This assertion is the only
    // one of the three that dies to those mutants.
    assert!(
        stderr.contains("same file"),
        "the refusal must be about aliasing, not something later; stderr:\n{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The same aliasing on the COMPARE path, which is the quieter half: there it made the gate read
/// back the run that had just overwritten the baseline and compare it to itself, reporting no
/// drift and exiting 0. Measured on the pre-fix binary with a baseline recording p0 at 100%
/// against a run that scored 0%: `0% | 0%`, zero flips, `0 FAIL`, exit 0.
#[test]
fn an_aliased_comparison_is_refused_before_the_baseline_can_be_overwritten() {
    let dir = scratch("compare");
    let baseline = seed_baseline(&dir);
    let path = baseline.display().to_string();

    let (code, stderr) = run(&["--baseline", &path, "--current-output", &path]);

    assert_ne!(
        code,
        Some(0),
        "an aliased comparison must fail; stderr:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&baseline).expect("read baseline"),
        SENTINEL,
        "the baseline was modified by a run that was refused"
    );
    // LOAD-BEARING — see the module note. Measured: with the alias check deleted, or with
    // `same_file` forced to false, the two assertions above still pass, because the binary then
    // fails at the card database with the baseline equally untouched. This assertion is the only
    // one of the three that dies to those mutants.
    assert!(
        stderr.contains("same file"),
        "the refusal must be about aliasing, not something later; stderr:\n{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The refusal block itself, executed.
///
/// An earlier round disclosed this as uncovered: the two predicates behind the refusal were unit
/// tested, but the block that acts on them lived in a binary no test ran, so deleting or
/// inverting it left the suite green. Reaching it looked like it needed a card database and a
/// real suite run — minutes of CI for a local-only command.
///
/// It does not. `CardDatabase::from_export` deserialises a map, so `{}` is a VALID empty
/// database, and a `--suite-filter` matching nothing selects no matchups, builds no decks and
/// plays no games. The run therefore reaches `recorded_games() == 0` and refuses, in about five
/// milliseconds, with no card data on disk.
///
/// The exit code is asserted as exactly 1 rather than merely non-zero, and that is what makes
/// this test see the block instead of an early death: argument and database failures exit 2, so
/// a mutant that stops the binary before the suite runs changes 1 to 2 and fails here.
#[test]
fn a_gameless_refresh_is_refused_by_the_block_and_leaves_the_baseline_untouched() {
    let dir = scratch("gameless");
    let data_arg = empty_card_db(&dir);
    let baseline = record_baseline(&dir, &data_arg, "suite-baseline.json");
    let trusted = std::fs::read_to_string(&baseline).expect("read baseline");

    let (code, stderr) = run(&[
        "--refresh-baseline",
        "--data-root",
        &data_arg,
        "--baseline",
        &baseline.display().to_string(),
        "--current-output",
        &dir.join("current.json").display().to_string(),
        "--suite-filter",
        "no-such-matchup",
    ]);

    assert_eq!(
        code,
        Some(1),
        "expected the refusal block's exit 1, not an earlier failure; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("recorded no games"),
        "the refusal must be the gameless one; stderr:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&baseline).expect("read baseline"),
        trusted,
        "a refused refresh must leave the baseline byte-identical"
    );
    // The staging file is an implementation detail of the transaction, but a leftover one is an
    // artefact someone later mistakes for a real baseline.
    let staging = baseline.with_file_name("suite-baseline.json.staging.json");
    assert!(
        !staging.exists(),
        "a refused refresh must not leave {} behind",
        staging.display()
    );
    // The consequence of leaving it, asserted rather than inferred: the staging path is now
    // RESERVED with `create_new`, so a leftover does not merely look like an artefact — it blocks
    // every later refresh. One refused run must not poison the next.
    let (next_code, next_stderr) = run(&[
        "--refresh-baseline",
        "--data-root",
        &data_arg,
        "--baseline",
        &baseline.display().to_string(),
        "--current-output",
        &dir.join("current2.json").display().to_string(),
        "--suite-filter",
        "red-mirror",
        "--games",
        "1",
    ]);
    assert_eq!(
        next_code,
        Some(0),
        "a refused refresh must not block the next one; stderr:\n{next_stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `--games 0` is refused at argument parse time, before any work at all.
///
/// The data root deliberately does not exist, so `failed to load card database` WOULD appear if
/// the run reached it — its absence is what makes the ordering claim a live signal rather than an
/// absence-shaped one.
#[test]
fn a_zero_game_refresh_is_refused_before_the_run_begins() {
    let dir = scratch("zero-games");
    let baseline = seed_baseline(&dir);
    let current = dir.join("current.json");
    std::fs::write(&current, CURRENT_MARKER).expect("seed current-output");

    let (code, stderr) = run(&[
        "--refresh-baseline",
        "--data-root",
        &dir.join("no-such-data-root").display().to_string(),
        "--baseline",
        &baseline.display().to_string(),
        "--current-output",
        &current.display().to_string(),
        "--games",
        "0",
    ]);

    assert_eq!(
        code,
        Some(2),
        "a zero-game refresh must be refused at the argument check; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("--games must be a positive integer"),
        "the refusal must name --games; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("failed to load card database"),
        "the refusal must precede the card database load; stderr:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&baseline).expect("read baseline"),
        SENTINEL,
        "the baseline was modified by a run that was refused"
    );
    assert_eq!(
        std::fs::read_to_string(&current).expect("read current-output"),
        CURRENT_MARKER,
        "--current-output was written by a run that was refused"
    );
    let staging = baseline.with_file_name("suite-baseline.json.staging.json");
    assert!(
        !staging.exists(),
        "a refused refresh must not leave {} behind",
        staging.display()
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The failure half of the refusal block, executed: a run whose matchup failed its own `Expected`
/// check must not become the baseline, or the next run compares equal to a blessed regression.
///
/// Fixture: with an empty card database, `--seed 20` gives red-mirror four games that all go to
/// p1, which is what makes `classify` fail the mirror. If the engine ever perturbs those games,
/// rescan seeds against this same matchup at `--games 4` and take any seed where red-mirror fails
/// its own check.
///
/// Exit code asserted as exactly 1, not merely non-zero: argument and database failures exit 2,
/// so a mutant that stops the binary before the suite runs fails here.
#[test]
fn a_refresh_whose_matchup_failed_is_refused_and_the_baseline_survives() {
    let dir = scratch("failing-matchup");
    let data_arg = empty_card_db(&dir);
    let baseline = record_baseline(&dir, &data_arg, "suite-baseline.json");
    let trusted = std::fs::read_to_string(&baseline).expect("read baseline");

    let (code, stderr) = run(&[
        "--refresh-baseline",
        "--data-root",
        &data_arg,
        "--baseline",
        &baseline.display().to_string(),
        "--current-output",
        &dir.join("current.json").display().to_string(),
        "--suite-filter",
        "red-mirror",
        "--games",
        "4",
        "--seed",
        "20",
    ]);

    assert_eq!(
        code,
        Some(1),
        "expected the refusal block's exit 1, not an earlier failure; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("matchup(s) failed their own suite check"),
        "the refusal must be the failing-matchup one; stderr:\n{stderr}"
    );
    // The per-matchup diagnostic, which is what distinguishes this refusal from the gameless one.
    assert!(
        stderr.contains("red-mirror: mirror imbalance"),
        "the refusal must name the matchup and its reason; stderr:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&baseline).expect("read baseline"),
        trusted,
        "a refused refresh must leave the baseline byte-identical"
    );
    let staging = baseline.with_file_name("suite-baseline.json.staging.json");
    assert!(
        !staging.exists(),
        "a refused refresh must not leave {} behind",
        staging.display()
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The other side of the transaction: an ACCEPTED refresh must actually promote the staged
/// report over the old baseline.
///
/// Without this, "a refused run leaves the baseline alone" is satisfied by a binary that never
/// writes a baseline at all — measured: deleting the promotion entirely left every other test in
/// this file green. The two directions have to be asserted together or the guard is
/// indistinguishable from a break.
///
/// Reachable at the same near-zero cost as the refusal tests, for a reason worth recording: an
/// empty card database still yields a playable (degenerate) matchup, so a single game completes
/// in about two milliseconds and the run is accepted. The baseline written here is meaningless
/// as a baseline — that is fine, because what is under test is the file transaction, not the
/// contents.
#[test]
fn an_accepted_refresh_promotes_the_staged_report_over_the_old_baseline() {
    let dir = scratch("accept");
    let data_arg = empty_card_db(&dir);
    let baseline = record_baseline(&dir, &data_arg, "suite-baseline.json");
    // Mark the recorded baseline so "was it replaced?" is answered by content, not by mtime.
    // A marked-but-valid file is required: an invalid one is now refused before the suite runs.
    let mut old: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&baseline).expect("read baseline"))
            .expect("baseline is JSON");
    old["git_sha"] = serde_json::json!("OLD-BASELINE-MARKER");
    let marked = serde_json::to_string(&old).expect("serialize baseline");
    std::fs::write(&baseline, &marked).expect("write baseline");
    // On the refresh path the refreshed baseline IS the run's report, so the suite must stage
    // beside the baseline rather than write through `--current-output`.
    let current = dir.join("current.json");
    std::fs::write(&current, CURRENT_MARKER).expect("seed current-output");

    let (code, stderr) = run(&[
        "--refresh-baseline",
        "--data-root",
        &data_arg,
        "--baseline",
        &baseline.display().to_string(),
        "--current-output",
        &current.display().to_string(),
        "--suite-filter",
        "red-mirror",
        "--games",
        "1",
    ]);

    assert_eq!(
        code,
        Some(0),
        "an accepted refresh must succeed; stderr:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&current).expect("read current-output"),
        CURRENT_MARKER,
        "an accepted refresh must not write through --current-output"
    );
    let written = std::fs::read_to_string(&baseline).expect("read baseline");
    assert_ne!(
        written, marked,
        "the accepted run was never promoted over the old baseline"
    );
    // PREMISE: what landed is the run's report, not any other file the process might have
    // written — so "changed" cannot pass by truncation or by a stray write.
    let report: serde_json::Value = serde_json::from_str(&written).expect("baseline is JSON");
    assert_eq!(report["games_per_matchup"], 1);
    assert_eq!(report["results"][0]["matchup_id"], "red-mirror");
    assert_eq!(
        report["results"][0]["games"].as_array().map(Vec::len),
        Some(1),
        "the promoted baseline must carry the game that was played"
    );
    let staging = baseline.with_file_name("suite-baseline.json.staging.json");
    assert!(
        !staging.exists(),
        "staging must not survive a successful refresh"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The alias no path comparison can see: one inode, two names.
///
/// `canonicalize` faithfully resolves a hard link to its own name, so the pre-fix path check
/// called these distinct and let the run proceed — and `write_report` opens `--current-output`
/// with `File::create`, which truncates the shared inode. Reading the baseline before the suite
/// runs saves the *verdict* from being computed against the run's own output, but it does not
/// save the *bytes*; only refusing does. Both halves are asserted here for that reason.
///
/// Compare mode rather than refresh, because it is the quieter half: refresh at least prints a
/// refusal, while compare would have gone on to report no drift and exit 0 against a baseline it
/// had just destroyed.
#[test]
fn a_hard_linked_output_is_refused_and_the_baseline_survives() {
    let dir = scratch("hardlink");
    // A RECORDED baseline and a real card database, because the destructive write is what this
    // test is about: with SENTINEL and no `--data-root` the run stops at the card database — or,
    // with only a data root, at the baseline parse — before `run_suite` writes anything, so the
    // exit code and the surviving bytes are both satisfied by a binary with no identity check at
    // all, and only the refusal text discriminates.
    let data_arg = empty_card_db(&dir);
    let baseline = record_baseline(&dir, &data_arg, "suite-baseline.json");
    let trusted = std::fs::read_to_string(&baseline).expect("read baseline");
    let link = dir.join("current-link.json");
    std::fs::hard_link(&baseline, &link).expect("hard link");

    // PREMISE: the fixture really is one file under two names. Without this the test would still
    // pass on a platform or filesystem that silently copied instead, having proven nothing.
    assert_ne!(
        std::fs::canonicalize(&baseline).expect("canonicalize baseline"),
        std::fs::canonicalize(&link).expect("canonicalize link"),
        "a hard link must present two distinct paths, or this test is not about hard links"
    );

    let (code, stderr) = run(&[
        "--baseline",
        &baseline.display().to_string(),
        "--current-output",
        &link.display().to_string(),
        "--data-root",
        &data_arg,
        "--suite-filter",
        "red-mirror",
        "--games",
        "1",
    ]);

    assert_eq!(
        code,
        Some(2),
        "a hard-linked output must be refused at the argument check; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("same file"),
        "the refusal must name the aliasing; stderr:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&baseline).expect("read baseline"),
        trusted,
        "the baseline was truncated through its hard link"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A baseline that exists but cannot be parsed must stop the refresh, not be overwritten by it.
///
/// The earlier code logged every read failure and carried on, so the run continued to `run_suite`
/// and renamed its staged report over the file. That destroys the evidence needed to work out WHY
/// the baseline was unreadable, and establishes the replacement from an unexamined prior state.
///
/// The exit code is asserted as exactly 2, which is what separates this from the gameless
/// refusal's 1: the point is that the run stops BEFORE the suite, not that it stops eventually.
/// A mutant that let the run proceed and refused later would leave the bytes intact on this
/// fixture too, and the code is the only assertion that tells the two apart.
#[test]
fn a_malformed_existing_baseline_is_not_replaced_by_a_refresh() {
    let dir = scratch("malformed");
    let data_arg = empty_card_db(&dir);
    let baseline = seed_baseline(&dir);

    let (code, stderr) = run(&[
        "--refresh-baseline",
        "--data-root",
        &data_arg,
        "--baseline",
        &baseline.display().to_string(),
        "--current-output",
        &dir.join("current.json").display().to_string(),
        "--suite-filter",
        "red-mirror",
        "--games",
        "1",
    ]);

    assert_eq!(
        code,
        Some(2),
        "a malformed baseline must stop the run before the suite; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("failed to load baseline"),
        "the refusal must name the baseline load; stderr:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&baseline).expect("read baseline"),
        SENTINEL,
        "an unreadable baseline must be preserved for diagnosis, not overwritten"
    );
    let staging = baseline.with_file_name("suite-baseline.json.staging.json");
    assert!(
        !staging.exists(),
        "a refused refresh must not leave {} behind",
        staging.display()
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The other side of that refusal: a MISSING baseline is the ordinary first-refresh case and must
/// still be allowed through.
///
/// Without this, narrowing the accepted error to `NotFound` is indistinguishable from refusing
/// every unreadable baseline including the absent one — which would make the first refresh on a
/// fresh checkout impossible. Measured: this is the arm that fails if the `NotFound` guard is
/// dropped in the refusing direction.
#[test]
fn a_missing_baseline_is_still_the_first_refresh_case() {
    let dir = scratch("firstrun");
    let data_arg = empty_card_db(&dir);
    let baseline = dir.join("does-not-exist-yet.json");
    assert!(!baseline.exists(), "fixture must start with no baseline");

    let (code, stderr) = run(&[
        "--refresh-baseline",
        "--data-root",
        &data_arg,
        "--baseline",
        &baseline.display().to_string(),
        "--current-output",
        &dir.join("current.json").display().to_string(),
        "--suite-filter",
        "red-mirror",
        "--games",
        "1",
    ]);

    assert_eq!(
        code,
        Some(0),
        "a first refresh with no baseline must succeed; stderr:\n{stderr}"
    );
    assert!(
        baseline.exists(),
        "the first refresh must create the baseline"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Control arm for the identity check specifically: two files that BOTH exist and are genuinely
/// different must not be called aliases.
///
/// `distinct_paths_are_not_treated_as_aliases` does not cover this — its output path does not
/// exist, so `same_inode` returns `None` and the path fallback answers. This is the only case
/// that reaches the inode comparison and expects `Some(false)`, so without it an implementation
/// that reported every existing pair as identical would pass the whole file while refusing every
/// real invocation of the gate.
///
/// Known gap, stated rather than implied: this does not pin the `dev` half of `(dev, ino)`. A
/// mutant comparing only `ino` survives every test here, because killing it needs two files on
/// different filesystems whose inode numbers collide, and inode allocation is not controllable
/// enough to construct that deterministically. The `dev` term is kept on correctness grounds —
/// dropping it can only ever produce a false ALIAS report, whose consequence is a refused run
/// rather than a destroyed baseline, so the unpinned direction is the safe one.
#[test]
fn two_existing_but_different_files_are_not_aliases() {
    let dir = scratch("distinct-existing");
    let baseline = seed_baseline(&dir);
    let current = dir.join("current.json");
    std::fs::write(&current, "{}").expect("seed current");

    // PREMISE: both really exist, so the run reaches the inode comparison rather than the
    // path fallback this test is not about.
    assert!(baseline.exists() && current.exists());

    let (_code, stderr) = run(&[
        "--baseline",
        &baseline.display().to_string(),
        "--current-output",
        &current.display().to_string(),
        "--data-root",
        &dir.join("no-such-data-root").display().to_string(),
    ]);

    assert!(
        !stderr.contains("same file"),
        "two distinct existing files must not be called aliases; stderr:\n{stderr}"
    );
    // PREMISE: the run proceeded past the argument check, so the assertion above is about
    // aliasing rather than about the process dying even earlier.
    assert!(
        stderr.contains("failed to load card database"),
        "expected the run to proceed to the card database; stderr:\n{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The third route to the same destruction, and the one no argument check can see: the staging
/// path the process derives for ITSELF.
///
/// `staging_path` is deterministic — `<baseline>.staging.json` — and `write_report` opened it by
/// name with `File::create` before any refresh guard ran. So an entry already sitting at that
/// path was followed (symlink) or shared (hard link), and the baseline was truncated by a write
/// nobody passed on the command line. `same_file` cannot help: it compares `--baseline` against
/// `--current-output`, and the staging path is neither.
///
/// Both alias kinds are exercised because they fail differently — `File::create` FOLLOWS a
/// symlink to its target, while a hard link IS the target — and a fix that reserved the path
/// against only one of them would leave the other live.
#[cfg(unix)]
#[test]
fn a_pre_existing_staging_alias_cannot_truncate_the_baseline() {
    for kind in ["symlink", "hardlink"] {
        let dir = scratch(&format!("staging-{kind}"));
        let data_arg = empty_card_db(&dir);
        let baseline = record_baseline(&dir, &data_arg, "suite-baseline.json");
        let trusted = std::fs::read_to_string(&baseline).expect("read baseline");
        let staging = baseline.with_file_name("suite-baseline.json.staging.json");
        if kind == "symlink" {
            std::os::unix::fs::symlink(&baseline, &staging).expect("symlink");
        } else {
            std::fs::hard_link(&baseline, &staging).expect("hard link");
        }

        // PREMISE: the alias really points at the baseline, so a write through it would land on
        // the file under test. Without this the fixture could be inert and the test vacuous.
        assert_eq!(
            std::fs::read_to_string(&staging).expect("read through alias"),
            trusted,
            "{kind}: the staging alias must resolve to the baseline"
        );

        // A refresh that would otherwise be ACCEPTED — the destructive case, because an accepted
        // run is the one that goes on to rename over the baseline.
        let (code, stderr) = run(&[
            "--refresh-baseline",
            "--data-root",
            &data_arg,
            "--baseline",
            &baseline.display().to_string(),
            "--current-output",
            &dir.join("current.json").display().to_string(),
            "--suite-filter",
            "red-mirror",
            "--games",
            "1",
        ]);

        assert_ne!(
            code,
            Some(0),
            "{kind}: a run that cannot reserve its staging file must not report success; \
             stderr:\n{stderr}"
        );
        assert_eq!(
            std::fs::read_to_string(&baseline).expect("read baseline"),
            trusted,
            "{kind}: the baseline was truncated through the staging alias"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Control arm. Every assertion above is satisfied by a binary that refuses everything, which
/// would break the gate far worse than the bug being fixed. Distinct paths must get past the
/// argument check — proven by the failure being about something LATER in the run (the card
/// database or the suite), never about aliasing.
#[test]
fn distinct_paths_are_not_treated_as_aliases() {
    let dir = scratch("distinct");
    let baseline = seed_baseline(&dir);
    let current = dir.join("current.json");

    let (_code, stderr) = run(&[
        "--baseline",
        &baseline.display().to_string(),
        "--current-output",
        &current.display().to_string(),
        "--data-root",
        &dir.join("no-such-data-root").display().to_string(),
    ]);

    assert!(
        !stderr.contains("same file"),
        "distinct paths must not be rejected as aliases; stderr:\n{stderr}"
    );
    // PREMISE: the run really did proceed past the argument check, so the assertion above is
    // about aliasing rather than about the process dying even earlier for some other reason.
    assert!(
        stderr.contains("failed to load card database"),
        "expected the run to proceed to the card database; stderr:\n{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A symlinked baseline names a file somewhere else, and a refresh must update THAT file.
///
/// The pre-staging code opened the baseline with `File::create`, which follows the link and
/// refreshes its target. Staging + `rename` does not: `rename` acts on the entry, so promoting
/// onto the supplied spelling replaces the link with a regular file and leaves the target on its
/// old bytes. Measured on the binary before the fix, over all four arms: exit 0,
/// "baseline refreshed at <link>", the link no longer a link, and the target byte-identical.
///
/// The arms are the ends of the class, not a sample. An absolute and a relative link target
/// separate a resolver that joins the link's own directory from one that does not; a dangling link
/// is the first-refresh-through-a-link case, where the target has to be CREATED, which is what
/// `File::create` did; and a two-hop chain separates a resolver that reads one link from one that
/// follows to the end. A hard-linked baseline is deliberately absent — a hard link has no target,
/// and `rename` breaking it instead of truncating the shared inode is the intended behaviour.
///
/// The target sits in a SUBDIRECTORY of the link so that "resolved" and "supplied" differ by more
/// than a file name: a staging file derived from the wrong one lands in the wrong directory.
///
/// `#[cfg(unix)]` because creating a symlink on Windows needs a privilege the test process is not
/// guaranteed to hold; this repository's CI is Linux (`ci.yml`, every job `runs-on: ubuntu-latest`),
/// so this arm does run there, and the existing
/// `a_pre_existing_staging_alias_cannot_truncate_the_baseline` sets the precedent.
#[cfg(unix)]
#[test]
fn a_symlinked_baseline_refreshes_its_target_and_stays_a_link() {
    for kind in ["absolute", "relative", "dangling", "chain"] {
        let dir = scratch(&format!("symlink-{kind}"));
        let data_arg = empty_card_db(&dir);
        std::fs::create_dir_all(dir.join("real")).expect("create target dir");
        let target = dir.join("real/baseline.json");
        let link = dir.join("link.json");

        // A marked, VALID baseline at the target for every arm but `dangling`, whose whole point is
        // that the target does not exist yet. An unparseable one would be refused before the suite
        // ran, and the assertions would then pass on a run that never reached the promotion.
        let marked = (kind != "dangling").then(|| {
            let recorded = record_baseline(&dir, &data_arg, "real/baseline.json");
            let mut old: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&recorded).expect("read baseline"))
                    .expect("baseline is JSON");
            old["git_sha"] = serde_json::json!("OLD-BASELINE-MARKER");
            let marked = serde_json::to_string(&old).expect("serialize baseline");
            std::fs::write(&recorded, &marked).expect("write baseline");
            marked
        });

        match kind {
            // Relative because a link's target resolves against the link's own directory: a
            // resolver that passes the raw target onward yields `real/baseline.json` relative to
            // the process cwd, which is not this file.
            "relative" => std::os::unix::fs::symlink("real/baseline.json", &link),
            // Two hops. A resolver that reads one link stops at `mid.json` and renames over THAT.
            "chain" => {
                let mid = dir.join("mid.json");
                std::os::unix::fs::symlink(&target, &mid).expect("symlink mid");
                std::os::unix::fs::symlink(&mid, &link)
            }
            _ => std::os::unix::fs::symlink(&target, &link),
        }
        .expect("symlink");

        // PREMISE: the link really resolves to the target the assertions read — and, on the
        // `dangling` arm, really resolves to nothing yet. Without this the fixture could be inert.
        assert_eq!(
            std::fs::read_to_string(&link).ok(),
            marked,
            "{kind}: the link must resolve to the target under test"
        );

        let (code, stderr) = run(&[
            "--refresh-baseline",
            "--data-root",
            &data_arg,
            "--baseline",
            &link.display().to_string(),
            "--current-output",
            &dir.join("current.json").display().to_string(),
            "--suite-filter",
            "red-mirror",
            "--games",
            "1",
        ]);

        // Reach guard. Every assertion below is also satisfied by a binary that refuses the run —
        // which would leave the link a link and the target untouched — so the ACCEPTED path has to
        // be the one that was taken.
        assert_eq!(
            code,
            Some(0),
            "{kind}: the refresh must be accepted; stderr:\n{stderr}"
        );
        // The defect, in the direction that made the refresh a no-op for the caller: the entry
        // named on the command line was replaced by a regular file.
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("stat link")
                .file_type()
                .is_symlink(),
            "{kind}: --baseline was a symlink and must still be one"
        );
        // The other half: the file the link designates carries THIS run's report. Asserted
        // positively rather than as "changed", so it covers the `dangling` arm, where the target
        // had to be created, and so a truncation cannot pass for an update.
        let written = std::fs::read_to_string(&target).expect("read the link's target");
        let report: serde_json::Value =
            serde_json::from_str(&written).expect("the target must hold a report");
        assert_eq!(report["games_per_matchup"], 1, "{kind}: {written}");
        assert_eq!(report["results"][0]["matchup_id"], "red-mirror", "{kind}");
        assert_ne!(
            report["git_sha"],
            serde_json::json!("OLD-BASELINE-MARKER"),
            "{kind}: the target still holds the old baseline"
        );
        assert!(
            !dir.join("real/baseline.json.staging.json").exists(),
            "{kind}: staging survived beside the resolved destination"
        );
        assert!(
            !dir.join("link.json.staging.json").exists(),
            "{kind}: staging survived beside the supplied spelling"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
