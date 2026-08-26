//! Tier 2 — §10 rows 1, 2, 14, 21 and 32, plus the two arms of rows 26 and 33 that need a
//! real pipeline run. Everything here shells `unshare --map-root-user --mount`.
//!
//! The dogfood manifests pin `[target].test = "pure_logic"`, NEVER `isolation_e2e` — this
//! file is what runs probe-pin, so pinning it would recurse.
//!
//! Rows 1, 2 and 21 execute their own DROP arms: each is defined as a mutation of the
//! isolation SCRIPT (drop the readback / compare the mutant with itself / copy instead of
//! mount), so the test derives the mutant script and runs it through the same entry point.
//!
//! # Every test here is `#[ignore]`d, and that is a measurement, not a preference
//!
//! GitHub's runners deny unprivileged user namespaces: `unshare --map-root-user --mount`
//! fails with `write failed /proc/self/uid_map: Operation not permitted`. Measured on
//! <https://github.com/phase-rs/phase/actions/runs/31646292055> — 14 of these 15 tests failed
//! there for that one reason, spread across all four test shards.
//!
//! The suite is ignored WHOLE rather than per failing test. The lone survivor
//! (`proj_missing_both_paths`) passes only because its manifest is refused at validation,
//! before `unshare` is ever reached — a property of that fixture, not of the test. Splitting
//! the suite on "does this manifest happen to abort early" creates a boundary that moves
//! silently the next time a fixture or a refusal order changes, and a Tier-2 test that stops
//! reaching Tier 2 is exactly the unmeasured-green this crate exists to refuse.
//!
//! `#[ignore]` and not a runtime capability check: a test that quietly no-ops when the
//! namespace is missing reports as PASSED. That is this crate's own BLOCKER defect wearing a
//! test harness. An ignored test reports as ignored.
//!
//! Run them where namespaces work:
//! `cargo test -p probe-pin --test isolation_e2e -- --ignored`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use probe_pin::isolate::{self, Run};
use probe_pin::manifest::{FilterMatch, Manifest, Target};
use probe_pin::mutate::Mount;
use probe_pin::verdict::{self, Verdict};
use probe_pin::{Abort, HarnessMarker};

mod common;

const PROD: &str = "crates/probe-pin/tests/fixtures/prod.txt";
/// Row 21 arm 2 WRITES this path. It is gitignored (`.gitignore:113` = `tmp/`), so the arm
/// cannot leak into the tree even when its assertion panics before a restore — and it is
/// never a tracked fixture a concurrent `nextest --workspace` reader might be reading.
const TMP: &str = "crates/probe-pin/tests/fixtures/tmp";

fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| probe_pin::target::workspace_root().expect("workspace root"))
}

/// Resolved once per process: the nested `cargo test --no-run` is the expensive part.
fn testbin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        probe_pin::target::resolve(root(), "probe-pin", "pure_logic").expect("test binary")
    })
}

fn target(filter: &str, secs: u64, env: &[(&str, &str)]) -> Target {
    let mut t = Manifest::load(&root().join("crates/probe-pin/tests/fixtures/dogfood.toml"))
        .unwrap()
        .target;
    t.filter = filter.to_string();
    t.filter_match = FilterMatch::Substring;
    t.timeout_secs = secs;
    for (k, v) in env {
        t.env.insert((*k).to_string(), (*v).to_string());
    }
    t
}

fn mutant(dir: &Path, name: &str, text: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

/// The mutant that makes `ppfixture_census` fail: one of the two marker lines is gone.
fn dropped_site() -> String {
    std::fs::read_to_string(root().join(PROD))
        .unwrap()
        .replace("SITE-TWO marker beta\n", "")
}

/// The shipping SCRIPT minus the lines whose leading token matches — how rows 1, 2 and 21
/// build their DROP mutants.
fn script_without(tokens: &[&str]) -> String {
    isolate::SCRIPT
        .lines()
        .filter(|l| !tokens.iter().any(|t| l.trim_start().starts_with(t)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn sha_of(rel: &str) -> String {
    probe_pin::sha256_hex(&std::fs::read(root().join(rel)).unwrap())
}

/// Every probe-pin child shells its own NESTED `cargo test --test pure_logic --no-run` into the
/// shared `CARGO_TARGET_DIR`; the three §7 wiring arms take the count of such children from four
/// to seven, so they run one at a time rather than contending on cargo's build lock.
///
/// The lock is IN-PROCESS, and that is the whole guarantee: it serializes these calls under
/// libtest — one process, threads — which is the venue the Tilt `probe-pin-e2e` resource uses
/// (`cargo test … -- --ignored`). It does NOT serialize across processes, so a nextest run would
/// contend instead. Measured, and deliberately not configured around: `cargo nextest list -p
/// probe-pin --test isolation_e2e` schedules ZERO of these tests, because they are `#[ignore]`d
/// and CI's `cargo nextest run --profile ci --workspace` never passes `--run-ignored`; forcing it
/// with `--run-ignored all` still ran 17/17 green in 4.4s against 3.2s under libtest. A
/// `probe-pin-serial` group in `.config/nextest.toml` would be configuration for a venue nothing
/// uses, and it would go stale silently.
///
/// This is hygiene, NOT MAJOR-3's fix: that flake's mechanism was isolated to the mutation harness (a
/// `cp -p` restore preserves the pre-mutation mtime, so cargo keeps the artifact built from the
/// mutant and the NEXT run measures the PREVIOUS mutant — reproduced deterministically, and
/// cleared by one `touch`). Poisoning is ignored on purpose: a panicking arm must report ITS OWN
/// failure rather than turn every later arm into a poison error that hides it.
fn probe_pin_bin(args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_probe-pin"));
    cmd.current_dir(root()).args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("probe-pin runs");
    (
        isolate::exit_rc(&out.status),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ------------------------------------------------------------------ row 1

/// Row 1: the mutant is REACHED at the target's path inside the namespace, and the `cmp`
/// readback is what proves it. Arm A is the pinned behaviour; arm B removes the mount and the
/// readback catches it; arm C removes the readback too and the same unmounted run reports a
/// clean PASS — which is the whole reason a `pass` verdict is only worth its readback.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn mount_reach() {
    let dir = tempfile::tempdir().unwrap();
    let mounts = vec![Mount {
        mutant: mutant(dir.path(), "prod.txt", &dropped_site()),
        target: root().join(PROD),
    }];
    let t = target("ppfixture_census", 60, &[]);
    let before = sha_of(PROD);

    // arm A — real mount
    let run = isolate::run(testbin(), &mounts, &t).unwrap();
    assert_eq!(run.rc, 101, "the mutant must be visible to the target");
    let obs = verdict::observe("P1", &run, &t, &mounts).expect("no instrument failure");
    assert!(verdict::all_anchors_in_one(
        &["CENSUS VIOLATED: expected 2 sites, got 1".to_string()],
        &obs.failed_captures
    ));
    assert_eq!(sha_of(PROD), before, "the on-disk file must be untouched");

    // arm B — the mount never happened; the readback catches it, and the abort reported is
    // MountNotReached and NOT the co-firing HarnessIncomplete { SuiteStarted }.
    let run = isolate::run_with_script(&script_without(&["mount --bind"]), testbin(), &mounts, &t)
        .unwrap();
    assert_eq!(run.rc, 97);
    assert!(
        run.stdout.trim().is_empty(),
        "a reached mount emits suite-started first"
    );
    assert!(run.stderr.contains("MOUNT NOT REACHED"));
    let e = verdict::observe("P1", &run, &t, &mounts).unwrap_err();
    assert!(matches!(e, Abort::MountNotReached { .. }), "ordering: {e}");
    assert!(!matches!(
        e,
        Abort::HarnessIncomplete {
            missing: HarnessMarker::SuiteStarted,
            ..
        }
    ));

    // arm C (DROP) — without the readback, the very same unmounted run reports a clean pass
    let run = isolate::run_with_script(
        &script_without(&["mount --bind", "cmp -s"]),
        testbin(),
        &mounts,
        &t,
    )
    .unwrap();
    assert_eq!(run.rc, 0, "DROP arm: no mount, no readback -> a FALSE pass");
    let obs = verdict::observe("P1", &run, &t, &mounts).unwrap();
    assert!(!obs.target_failed);
    assert_eq!(sha_of(PROD), before);
}

// ------------------------------------------------------------------ row 2

/// Row 2: a `Prepend` mutant reaches the target too. The census reports 202 sites under a
/// real 200-line pad, and 2 when the mount is dropped.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn pad_reach() {
    let dir = tempfile::tempdir().unwrap();
    let padded = format!(
        "{}{}",
        "// pad marker\n".repeat(200),
        std::fs::read_to_string(root().join(PROD)).unwrap()
    );
    let mounts = vec![Mount {
        mutant: mutant(dir.path(), "prod.txt", &padded),
        target: root().join(PROD),
    }];
    let t = target("ppfixture_census", 60, &[]);
    let before = sha_of(PROD);

    let run = isolate::run(testbin(), &mounts, &t).unwrap();
    let obs = verdict::observe("P2", &run, &t, &mounts).unwrap();
    assert!(
        verdict::all_anchors_in_one(
            &["CENSUS VIOLATED: expected 2 sites, got 202".to_string()],
            &obs.failed_captures
        ),
        "the 200-line pad must be visible at the target's path"
    );

    // The readback catches an unmounted Prepend mutant too. This arm derives its script from
    // the SHIPPING one, so deleting the `cmp` line from `isolate::SCRIPT` flips it — which is
    // row 2's DROP (M-e: the readback is a single per-file `cmp` with no per-kind branch, so
    // this row exercises row 1's mutation on a Prepend mutant).
    let run = isolate::run_with_script(&script_without(&["mount --bind"]), testbin(), &mounts, &t)
        .unwrap();
    assert_eq!(
        run.rc, 97,
        "an unmounted pad must be caught by the readback"
    );
    assert!(matches!(
        verdict::observe("P2", &run, &t, &mounts).unwrap_err(),
        Abort::MountNotReached { .. }
    ));

    // and with the readback removed as well, the same unmounted run is a silent pass
    let run = isolate::run_with_script(
        &script_without(&["mount --bind", "cmp -s"]),
        testbin(),
        &mounts,
        &t,
    )
    .unwrap();
    assert_eq!(run.rc, 0);
    assert!(
        !verdict::observe("P2", &run, &t, &mounts)
            .unwrap()
            .target_failed
    );
    assert_eq!(sha_of(PROD), before);
}

// ------------------------------------------------------------------ row 14

/// Row 14: a hung target is killed and NAMED. Without `timeout` the run would hang past CI's
/// 240 s hard kill; with `timeout 0` it would never fire. The abort reported is
/// `TargetTimedOut` and not the co-firing `HarnessIncomplete { SuiteFinished }`.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn timeout() {
    let t = target("ppfixture_hang", 3, &[("PP_FIXTURE", "hang")]);
    let run = isolate::run(testbin(), &[], &t).unwrap();
    assert_eq!(run.rc, 124);
    let e = verdict::observe("P1", &run, &t, &[]).unwrap_err();
    assert!(
        matches!(e, Abort::TargetTimedOut { secs: 3, .. }),
        "ordering: {e}"
    );
    assert!(!matches!(e, Abort::HarnessIncomplete { .. }));

    // positive: a fast target under the SAME timeout passes
    let t = target("ppfixture_census", 3, &[]);
    let run = isolate::run(testbin(), &[], &t).unwrap();
    assert_eq!(run.rc, 0);
    assert!(verdict::observe("P1", &run, &t, &[]).is_ok());
}

// ------------------------------------------------------------------ row 21

/// Row 21: probe-pin never writes the files it mutates. TWO arms, because a one-armed row
/// reports the same outcome with and without the sha compare — it contains no phenomenon.
///
/// Arm 2's write goes to a GITIGNORED path AND the fingerprint set names that path: the two
/// move together, because `TreeMutated` only fires on a file in the fingerprint set, and that
/// set IS the manifest's mutated-file list. Relocating the write alone would stop the row
/// firing and silently re-vacuate it.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn tree() {
    let dir = tempfile::tempdir().unwrap();
    let t = target("ppfixture_census", 60, &[]);

    // arm 1 — a bind mount leaves the on-disk bytes alone
    let files = vec![PROD.to_string()];
    let mounts = vec![Mount {
        mutant: mutant(dir.path(), "prod.txt", &dropped_site()),
        target: root().join(PROD),
    }];
    let before = isolate::fingerprint(root(), &files).unwrap();
    isolate::run(testbin(), &mounts, &t).unwrap();
    let after = isolate::fingerprint(root(), &files).unwrap();
    assert_eq!(before, after);
    assert_eq!(isolate::verify_fingerprint(&before, &after), Ok(()));

    // arm 2 — a test-only "materialize" (copy over the real path instead of mounting it)
    std::fs::create_dir_all(root().join(TMP)).unwrap();
    let rel = format!("{TMP}/prod.txt");
    std::fs::write(
        root().join(&rel),
        "SITE-ONE marker alpha\nSITE-TWO marker beta\n",
    )
    .unwrap();
    let files = vec![rel.clone()];
    let mounts = vec![Mount {
        mutant: mutant(dir.path(), "arm2.txt", &dropped_site()),
        target: root().join(&rel),
    }];
    let before = isolate::fingerprint(root(), &files).unwrap();
    isolate::run_with_script(
        &isolate::SCRIPT.replace("mount --bind", "cp"),
        testbin(),
        &mounts,
        &t,
    )
    .unwrap();
    let after = isolate::fingerprint(root(), &files).unwrap();
    assert_ne!(
        before, after,
        "the copy must actually have written the tree"
    );
    let e = isolate::verify_fingerprint(&before, &after).unwrap_err();
    assert!(matches!(e, Abort::TreeMutated { .. }), "{e}");
    std::fs::remove_file(root().join(&rel)).ok();
}

// ------------------------------------------------------------------ row 32

/// Row 32: `check` catches drift in BOTH directions, and the exit codes discriminate — 1 for
/// drift or a verdict mismatch, 2 for rot (the pinned code moved so far the probe can no
/// longer be applied).
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn drift() {
    let dir = root().join(TMP);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = dir.join("drift.toml");
    let block_file = dir.join("drift_block.md");
    let manifest_arg = manifest
        .strip_prefix(root())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let base = std::fs::read_to_string(root().join("crates/probe-pin/tests/fixtures/dogfood.toml"))
        .unwrap()
        .replace(
            "crates/probe-pin/tests/fixtures/dogfood_block.md",
            "crates/probe-pin/tests/fixtures/tmp/drift_block.md",
        );
    std::fs::write(&manifest, &base).unwrap();
    std::fs::write(
        &block_file,
        "prose above\n// PROBE-PIN:BEGIN placeholder\n// PROBE-PIN:END\nprose below\n",
    )
    .unwrap();

    // (c) write, then an untouched check -> 0
    assert_eq!(probe_pin_bin(&["run", "--write", &manifest_arg], &[]).0, 0);
    let written = std::fs::read_to_string(&block_file).unwrap();
    assert!(written.starts_with("prose above\n") && written.ends_with("prose below\n"));
    assert_eq!(probe_pin_bin(&["check", &manifest_arg], &[]).0, 0);

    // (a) hand-edit a cell -> 1. The edit is the SAME LENGTH as what it replaces, so a
    // length-only comparison reports the tampered block as clean.
    let edited = written.replace("P1_drop_second_site", "P1_drop_second_SITE");
    assert_eq!(edited.len(), written.len());
    assert_ne!(edited, written);
    std::fs::write(&block_file, edited).unwrap();
    let (rc, err) = probe_pin_bin(&["check", &manifest_arg], &[]);
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("the generated block is stale"), "{err}");
    std::fs::write(&block_file, &written).unwrap();

    // (b) the verdict flips -> 1. P2's mutation still makes the target fail; the manifest now
    // says it should pass, so the run has a measured verdict that contradicts its expectation.
    let flipped = base.replace(
        "  outcome = \"fail\"\n  anchor  = [\"CENSUS VIOLATED: expected 2 sites, got 202\"]",
        "  outcome = \"pass\"",
    );
    assert_ne!(flipped, base, "the (b) arm's edit must actually apply");
    std::fs::write(&manifest, &flipped).unwrap();
    let (rc, err) = probe_pin_bin(&["run", "--write", &manifest_arg], &[]);
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("VERDICT MISMATCH"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&block_file).unwrap(),
        written,
        "a non-zero exit must never splice"
    );

    // (d) the pinned code moved -> the probe cannot be applied -> 2
    std::fs::write(&manifest, base.replace("SITE-TWO marker beta", "SITE-GONE")).unwrap();
    let (rc, err) = probe_pin_bin(&["check", &manifest_arg], &[]);
    assert_eq!(rc, 2, "{err}");
    assert!(err.contains("'find' matched 0 times"), "{err}");

    std::fs::remove_file(&manifest).ok();
    std::fs::remove_file(&block_file).ok();
}

// ------------------------------------------------------------------ rows 26, 33 (binary arms)

/// Row 26's both-paths arm: `run --write` and `check` BOTH abort when a declared projection's
/// tool cannot be run. Neither emits a block with the sentence missing.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn proj_missing_both_paths() {
    let missing = root().join("crates/probe-pin/tests/fixtures/astgrep_does_not_exist");
    let env = [("PROBE_PIN_ASTGREP", missing.to_str().unwrap())];
    for args in [
        vec![
            "run",
            "--write",
            "crates/probe-pin/tests/fixtures/projection.toml",
        ],
        vec!["check", "crates/probe-pin/tests/fixtures/projection.toml"],
    ] {
        let (rc, err) = probe_pin_bin(&args, &env);
        assert_eq!(rc, 2, "{err}");
        assert!(
            err.contains("will not emit a block with its projection sentences missing"),
            "{err}"
        );
    }
}

/// Row 33's ordering arm: the execution floor is evaluated on the CONTROL first, so a rotted
/// filter aborts naming `P0_control` and no probe is ever run. Under the trivialized ordering
/// (check every probe but not the control) the run still exits 2, but the control has already
/// been certified on zero tests and the abort names the wrong probe.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn no_tests_selected_ordering() {
    let dir = root().join(TMP);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = dir.join("rotted.toml");
    std::fs::write(
        &manifest,
        std::fs::read_to_string(root().join("crates/probe-pin/tests/fixtures/dogfood.toml"))
            .unwrap()
            .replace(
                "filter       = \"ppfixture_census\"",
                "filter       = \"ppfixture_census_OLDNAME\"",
            ),
    )
    .unwrap();
    let arg = manifest
        .strip_prefix(root())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // MAJOR-3 (impl review): this arm flaked ONCE in ~63 suite runs with text byte-identical to
    // row 33's own DROP signature (the `test_count` floor deleted), so a bare `assert_eq!(rc, 2)`
    // reports "the floor regressed" and "a one-off" in the SAME bytes. The one-off's mechanism
    // has since been reproduced — a mutation harness restoring with `cp -p` leaves cargo holding
    // the artifact built from the previous mutant — but the COLLISION is repaired here, because
    // any future one-off collides the same way: on failure the arm re-measures and names which
    // one it is. It never goes green on a one-off; it reports as one.
    let (rc, err) = probe_pin_bin(&["run", &arg], &[]);
    if rc != 2 {
        let again: Vec<i32> = (0..2)
            .map(|_| probe_pin_bin(&["run", &arg], &[]).0)
            .collect();
        let which = if again.iter().all(|r| *r != 2) {
            "REGRESSION (reproduced 3/3): the execution floor is not firing on the control"
        } else {
            "FLAKE (1 of 3): the re-runs aborted on the control as specified — MAJOR-3's residual, NOT a floor regression"
        };
        panic!("probe-pin: {which}. exit: first={rc}, re-runs={again:?}, expected 2\n{err}");
    }
    assert!(err.contains("P0_control MEASURED NOTHING"), "{err}");
    assert!(err.contains("selected ZERO tests"), "{err}");
    assert!(err.contains("INSTRUMENT FAILURE"), "{err}");
    assert!(
        !err.contains("P1_drop_second_site") && !err.contains("P2_pad"),
        "the pipeline must stop at the control: {err}"
    );
    std::fs::remove_file(&manifest).ok();
}

// ------------------------------------------------ §7 wiring: steps 7, 9 and 10 (MAJOR-2)
//
// Rows 10, 21 and §8's control-failed row pin the FUNCTIONS. These three arms pin the CALLS:
// each drives the real binary end-to-end on a manifest that reaches only that step, and each
// was revert-probed by deleting its step from `main.rs` (measured: rc 2 -> rc 0, all three).
// No production code participates in them beyond the pipeline itself.

/// §7 step 9 is CALLED. `manifest::validate` compares anchor LISTS to each other, so P1's list
/// being a SUBSET of P2's captured failure is invisible to it; only the all-pairs scan over the
/// captures sees it, and that scan runs at exactly one call site.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn wiring_discriminate() {
    let (rc, err) = probe_pin_bin(
        &["run", "crates/probe-pin/tests/fixtures/collide.toml"],
        &[],
    );
    assert_eq!(rc, 2, "{err}");
    assert!(
        err.contains("These two probes are not distinguished"),
        "{err}"
    );
    assert!(
        err.contains("P1_drop_second_site") && err.contains("P2_pad_reaches_target"),
        "{err}"
    );
}

/// §7 step 10 is CALLED — invariant (a)'s runtime enforcement. The manifest's target writes an
/// unmounted path during the CONTROL run, which is a real write to the measured tree.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn wiring_verify_fingerprint() {
    std::fs::create_dir_all(root().join(TMP)).unwrap();
    let file = root().join(TMP).join("tree_write.txt");
    std::fs::write(&file, "ORIGINAL\n").unwrap();

    let (rc, err) = probe_pin_bin(
        &["run", "crates/probe-pin/tests/fixtures/treewrite.toml"],
        &[],
    );
    // reach guard: without the write, the step has nothing to detect and the arm is vacuous
    assert_ne!(
        std::fs::read_to_string(&file).unwrap(),
        "ORIGINAL\n",
        "the target must actually have written the tree: {err}"
    );
    assert_eq!(rc, 2, "{err}");
    assert!(err.contains("TREE MUTATED"), "{err}");
    assert!(err.contains("tree_write.txt"), "{err}");
    std::fs::remove_file(&file).ok();
}

/// §7 step 7's `ControlFailed` branch is CALLED: a control that fails on the unmodified tree is
/// a broken instrument, and §8's control-failed row is otherwise unreachable.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn wiring_control_failed() {
    let (rc, err) = probe_pin_bin(
        &["run", "crates/probe-pin/tests/fixtures/control_fails.toml"],
        &[],
    );
    assert_eq!(rc, 2, "{err}");
    assert!(
        err.contains("the control probe 'P0_control' (no mounts, unmodified tree) FAILED"),
        "{err}"
    );
    assert!(
        err.contains("every other verdict in this run is meaningless"),
        "{err}"
    );
}

/// §7 step 7 goes through `verdict::verdict` (final review-impl, MAJOR-2). Step 7 used to
/// CONSTRUCT `Verdict::Pass { mounts_reached: 0 }` for the control by hand, so the one probe
/// whose whole job is "the instrument works" was the one probe outside the crate's verdict
/// authority: a control whose expectation the run refutes rendered `| fail | pass |` in a
/// spliceable block and exited 0. The complement — a control declared `fail` that really does
/// fail — is `wiring_control_failed` above: an instrument failure, exit 2, no verdict at all.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn wiring_control_goes_through_the_verdict_authority() {
    let (rc, err) = probe_pin_bin(
        &[
            "run",
            "crates/probe-pin/tests/fixtures/control_expect_fail.toml",
        ],
        &[],
    );
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("VERDICT MISMATCH"), "{err}");
    assert!(
        err.contains("P0_control expected outcome = \"fail\" but the target PASSED"),
        "{err}"
    );
}

/// A manifest under the gitignored tmp dir, plus its workspace-relative argument.
fn tmp_manifest(name: &str, text: &str) -> (PathBuf, String) {
    let dir = root().join(TMP);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    let arg = path
        .strip_prefix(root())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    (path, arg)
}

/// The execution floor END TO END — the BLOCKER's exact repro, flipped. A `#[ignore]` on the
/// pinned test needs no manifest edit at all, so before this floor the digest, the block and the
/// Tilt resource stayed green forever for a tree nothing measured. Measured on the shipping
/// binary before the fix: exit 0, and a `| pass | pass | mount reached: 1 file(s) |` row.
///
/// Both routes run through the real pipeline, so this pins the CALL as well as the condition.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn execution_floor_aborts_a_zero_run() {
    let dogfood =
        std::fs::read_to_string(root().join("crates/probe-pin/tests/fixtures/dogfood.toml"))
            .unwrap();
    // Each edit is asserted to APPLY, by the same rule the hostile loop below already uses. The
    // reach guard cannot see a no-op edit: an UNEDITED dogfood.toml also exits 0 here, so three
    // replaces silently becoming no-ops (one space of formatting drift is enough) would leave
    // this test green while both hostile arms measured a manifest of a different shape.
    let mut base = dogfood.clone();
    for (what, find, replace) in [
        // a probe whose mutant leaves the census passing: the shape that renders a green `pass`
        // row, which is what a skipped run forges
        (
            "the census-preserving mutation",
            "  find    = \"SITE-TWO marker beta\\n\"\n  replace = \"\"",
            "  find    = \"alpha\"\n  replace = \"gamma\"",
        ),
        (
            "its assert_count",
            "  text  = \"marker\"\n  count = 1",
            "  text  = \"gamma\"\n  count = 1",
        ),
        (
            "P1's expectation",
            "  outcome = \"fail\"\n  anchor  = [\"CENSUS VIOLATED: expected 2 sites, got 1\",\n             \"text: \\\"SITE-ONE marker alpha\\\\n\\\"\"]",
            "  outcome = \"pass\"",
        ),
    ] {
        let edited = base.replace(find, replace);
        assert_ne!(
            edited, base,
            "{what}: the edit must apply — dogfood.toml's formatting drifted, and without it \
             this test measures the committed manifest instead of the floor's shape"
        );
        base = edited;
    }
    // the reach guard: this manifest must be GREEN when the target really runs, or every arm
    // below aborts for a reason that has nothing to do with the floor
    let (_, arg) = tmp_manifest("floor_base.toml", &base);
    let (rc, err) = probe_pin_bin(&["run", &arg], &[]);
    assert_eq!(rc, 0, "the benign arm must pass: {err}");

    for (name, edit, route) in [
        (
            "floor_ignored.toml",
            base.replace(
                "filter       = \"ppfixture_census\"",
                "filter       = \"ppfixture_ignored\"",
            ),
            "#[ignore] on the pinned test",
        ),
        (
            "floor_bench.toml",
            base.replace(
                "timeout_secs = 60",
                "timeout_secs = 60\nargs         = [\"--bench\"]",
            ),
            "--bench through [target].args",
        ),
    ] {
        assert_ne!(edit, base, "{route}: the edit must apply");
        let (_, arg) = tmp_manifest(name, &edit);
        let (rc, err) = probe_pin_bin(&["run", &arg], &[]);
        assert_eq!(rc, 2, "{route} must abort, got {rc}: {err}");
        assert!(err.contains("MEASURED NOTHING"), "{route}: {err}");
        assert!(err.contains("was SKIPPED"), "{route}: {err}");
        // and it stops at the CONTROL, so no probe's verdict is rendered from a skipped run
        assert!(err.contains("P0_control"), "{route}: {err}");
        assert!(!err.contains("PROBE-PIN:BEGIN"), "{route}: {err}");
    }
    for name in ["floor_base.toml", "floor_ignored.toml", "floor_bench.toml"] {
        std::fs::remove_file(root().join(TMP).join(name)).ok();
    }
}

/// §7 step 1b is CALLED. `[output].file` through an IN-TREE SYMLINK is lexically clean — every
/// component is `Normal` — so `validate` accepts it and only the realpath containment assert at
/// the new call site can refuse it. Measured on the shipping binary before the fix: exit 0, and
/// the block spliced into a file outside the repository.
///
/// Revert-probe: delete the `validate_paths` call from `main.rs` and this arm goes to rc 0 with
/// the block written at the symlink's target.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn wiring_validate_paths() {
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("escape.md");
    std::fs::write(
        &victim,
        "prose above\n// PROBE-PIN:BEGIN placeholder\n// PROBE-PIN:END\nprose below\n",
    )
    .unwrap();
    std::fs::create_dir_all(root().join(TMP)).unwrap();
    let link = root().join(TMP).join("escape_link");
    std::fs::remove_file(&link).ok();
    common::symlink(outside.path(), &link).unwrap();

    let text = std::fs::read_to_string(root().join("crates/probe-pin/tests/fixtures/dogfood.toml"))
        .unwrap()
        .replace(
            "crates/probe-pin/tests/fixtures/dogfood_block.md",
            "crates/probe-pin/tests/fixtures/tmp/escape_link/escape.md",
        );
    let (_, arg) = tmp_manifest("escape_out.toml", &text);
    let before = std::fs::read_to_string(&victim).unwrap();

    let (rc, err) = probe_pin_bin(&["run", "--write", &arg], &[]);
    assert_eq!(rc, 2, "{err}");
    assert!(err.contains("[output].file"), "{err}");
    assert!(err.contains("is not workspace-relative"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        before,
        "nothing may be written outside the workspace"
    );

    // positive: the same manifest naming a real in-tree output still runs to a block
    let (_, arg) = tmp_manifest(
        "escape_out_ok.toml",
        &text.replace(
            "crates/probe-pin/tests/fixtures/tmp/escape_link/escape.md",
            "crates/probe-pin/tests/fixtures/tmp/contained_block.md",
        ),
    );
    std::fs::write(
        root().join(TMP).join("contained_block.md"),
        "prose above\n// PROBE-PIN:BEGIN placeholder\n// PROBE-PIN:END\nprose below\n",
    )
    .unwrap();
    let (rc, err) = probe_pin_bin(&["run", "--write", &arg], &[]);
    assert_eq!(rc, 0, "the contained path must still be writable: {err}");

    std::fs::remove_file(&link).ok();
    for name in [
        "escape_out.toml",
        "escape_out_ok.toml",
        "contained_block.md",
    ] {
        std::fs::remove_file(root().join(TMP).join(name)).ok();
    }
}

/// §7 step 1b's block-text half is CALLED. An anchor carrying the END marker is lexically clean
/// everywhere else — it is not an id, not a path, and `cell()` escapes only `|`. Measured on the
/// shipping binary before the refusal: `run --write` exited 0, the spliced file then held TWO
/// END markers, and every subsequent `check` aborted with `expected exactly one
/// PROBE-PIN:BEGIN/END pair … found 1/2`, so the pin could only be recovered by hand.
///
/// Revert-probe: delete the `validate_block_text` call from `main.rs` and the hostile arm goes
/// to rc 0 with two END markers in the written file — which the arm asserts on directly.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn wiring_validate_block_text() {
    let block = root().join(TMP).join("marker_block.md");
    let manifest = |anchor: &str| {
        format!(
            "version = 1\n\
             [target]\n\
             mode         = \"runtime-read\"\n\
             package      = \"probe-pin\"\n\
             test         = \"pure_logic\"\n\
             filter       = \"ppfixture_census\"\n\
             filter_match = \"substring\"\n\
             timeout_secs = 60\n\
             [output]\n\
             file   = \"crates/probe-pin/tests/fixtures/tmp/marker_block.md\"\n\
             marker = \"PROBE-PIN\"\n\
             [[probe]]\n\
             id = \"P0_control\"\n\
             [probe.expect]\n\
             outcome = \"pass\"\n\
             [[probe]]\n\
             id = \"P1_marker_anchor\"\n\
             [[probe.mutation]]\n\
             kind   = \"prepend\"\n\
             files  = [\"crates/probe-pin/tests/fixtures/prod.txt\"]\n\
             text   = \"PROBE-PIN:END marker\\n\"\n\
             repeat = 1\n\
             [probe.expect]\n\
             outcome = \"fail\"\n\
             anchor  = [\"{anchor}\"]\n"
        )
    };
    let fresh = || {
        std::fs::create_dir_all(root().join(TMP)).unwrap();
        std::fs::write(
            &block,
            "prose above\n// PROBE-PIN:BEGIN placeholder\n// PROBE-PIN:END\nprose below\n",
        )
        .unwrap();
    };
    let end_markers = || {
        std::fs::read_to_string(&block)
            .unwrap()
            .matches("PROBE-PIN:END")
            .count()
    };

    // hostile: the anchor names the END marker, and the anchor really does hit — the mutant
    // prepends that exact text, so this is refused for being marker-unsafe, not for missing
    fresh();
    let (_, arg) = tmp_manifest("marker_attack.toml", &manifest("PROBE-PIN:END"));
    let (rc, err) = probe_pin_bin(&["run", "--write", &arg], &[]);
    assert_eq!(rc, 2, "{err}");
    assert!(err.contains("contains the marker tag"), "{err}");
    assert!(err.contains("P1_marker_anchor"), "{err}");
    assert_eq!(end_markers(), 1, "a refused run must not write");

    // reach guard: the SAME manifest with a marker-free anchor runs to a spliced block, so the
    // refusal above is about the marker text and not about this manifest's shape
    fresh();
    let (_, arg) = tmp_manifest(
        "marker_ok.toml",
        &manifest("CENSUS VIOLATED: expected 2 sites, got 3"),
    );
    let (rc, err) = probe_pin_bin(&["run", "--write", &arg], &[]);
    assert_eq!(rc, 0, "the benign twin must still write: {err}");
    assert_eq!(end_markers(), 1, "exactly one END marker survives a write");
    assert_eq!(probe_pin_bin(&["check", &arg], &[]).0, 0);

    for name in ["marker_attack.toml", "marker_ok.toml", "marker_block.md"] {
        std::fs::remove_file(root().join(TMP).join(name)).ok();
    }
}

/// A script-level failure is NAMED, not inferred from a stderr prefix. `mount`, `cmp` and
/// `timeout` are resolved through `PATH` INSIDE the namespace under `set -eu`, so a PATH without
/// them makes the shell die before `exec` — empty stdout, non-zero code, which is byte-for-byte
/// the shape of a target that died before its first libtest record.
///
/// DROP arm: the same run with the script's reserved-code `trap` removed is the measurement
/// that motivated it — exit 127, and `HarnessIncomplete`, whose message tells the operator to
/// raise `RUST_MIN_STACK`, which cannot make `timeout` exist.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn script_failure_is_named_not_inferred() {
    fn on_path(tool: &str) -> PathBuf {
        std::env::var("PATH")
            .unwrap()
            .split(':')
            .map(|dir| Path::new(dir).join(tool))
            .find(|p| p.exists())
            .unwrap_or_else(|| panic!("{tool} must be on PATH for this test to mean anything"))
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // enough to reach the script, and nothing the script needs after that
    for tool in ["unshare", "bash"] {
        common::symlink(on_path(tool), bin.join(tool)).unwrap();
    }
    for missing in ["mount", "cmp", "timeout"] {
        assert!(
            !bin.join(missing).exists(),
            "reach guard: {missing} must be absent from the constructed PATH"
        );
    }
    let t = target("ppfixture_census", 60, &[("PATH", bin.to_str().unwrap())]);

    let run = isolate::run(testbin(), &[], &t).unwrap();
    assert_eq!(run.rc, 96, "stderr: {}", run.stderr);
    assert!(
        run.stdout.trim().is_empty(),
        "the target never ran: {}",
        run.stdout
    );
    let e = verdict::observe("P1", &run, &t, &[]).unwrap_err();
    assert!(matches!(e, Abort::IsolationScriptFailed { .. }), "{e}");
    assert!(
        !e.to_string().contains("RUST_MIN_STACK"),
        "the remedy must not name a stack size: {e}"
    );

    // DROP — the reserved code removed from the SHIPPING script
    let dropped = script_without(&["trap"]);
    assert_ne!(dropped, isolate::SCRIPT, "the DROP edit must apply");
    let run = isolate::run_with_script(&dropped, testbin(), &[], &t).unwrap();
    assert_eq!(run.rc, 127, "stderr: {}", run.stderr);
    let e = verdict::observe("P1", &run, &t, &[]).unwrap_err();
    assert!(
        matches!(
            e,
            Abort::HarnessIncomplete {
                missing: HarnessMarker::SuiteStarted,
                ..
            }
        ),
        "{e}"
    );
    assert!(e.to_string().contains("RUST_MIN_STACK"), "{e}");

    // positive: the same target with the ambient PATH still runs to a clean observation
    let t = target("ppfixture_census", 60, &[]);
    assert!(verdict::observe("P1", &isolate::run(testbin(), &[], &t).unwrap(), &t, &[]).is_ok());
}

/// The dogfood: probe-pin runs its own committed manifest against its own committed block.
/// This is the same invocation the Tilt `probe-pin-check` resource makes.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn dogfood_check() {
    let (rc, err) = probe_pin_bin(
        &["check", "crates/probe-pin/tests/fixtures/dogfood.toml"],
        &[],
    );
    assert_eq!(rc, 0, "{err}");
}

/// The isolation model itself: a run under `unshare` never sees the ambient environment, and
/// `Run`'s streams are never merged.
#[test]
#[ignore = "Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`."]
fn streams_are_separate() {
    let t = target("ppfixture_census", 60, &[]);
    let run: Run = isolate::run(testbin(), &[], &t).unwrap();
    assert!(
        run.stdout.lines().next().unwrap().contains("\"suite\""),
        "stdout must be a pure record stream: {:?}",
        run.stdout.lines().next()
    );
    assert!(
        run.stderr.is_empty(),
        "streams are never merged: {:?}",
        run.stderr
    );
    assert_eq!(
        verdict::verdict(
            &Manifest::load(&root().join("crates/probe-pin/tests/fixtures/dogfood.toml"))
                .unwrap()
                .probes[0],
            &verdict::observe("P0_control", &run, &t, &[]).unwrap(),
            0
        ),
        Ok(Verdict::Pass { mounts_reached: 0 })
    );
}
