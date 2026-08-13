//! probe-pin — pins probe-derived claims to a regenerable, digest-stamped block in source.
//!
//! A probe is a mutation of the tree plus an expectation about the target test's outcome.
//! probe-pin materializes every mutant OUTSIDE the workspace, bind-mounts it over the real
//! path inside an unprivileged mount namespace, runs the target with libtest's JSON record
//! format, and renders the measured verdicts into a `PROBE-PIN:BEGIN…END` block.
//!
//! Regenerate a block: `cargo probe-pin run --write <manifest>` (alias in `.cargo/config.toml`).
//! Verify one:         `cargo probe-pin check <manifest>`

use std::path::PathBuf;

pub mod block;
pub mod isolate;
pub mod manifest;
pub mod mutate;
pub mod project;
pub mod target;
pub mod verdict;

use manifest::{FilterMatch, RunMode};

/// The two libtest suite records whose presence proves the harness ran to completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum HarnessMarker {
    SuiteStarted,
    SuiteFinished,
}

impl std::fmt::Display for HarnessMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SuiteFinished => "emitted libtest's suite-started record but never its suite-finished record — the harness started and died mid-run",
            Self::SuiteStarted => "never emitted libtest's suite-started record — the harness died before it reported a test count",
        })
    }
}

/// WHY a run executed no test body. The PARAMETER of the one "this run measured nothing"
/// abort, not a second abort: the refusal, its consequence and its exit code are identical in
/// both shapes, and only the sentence naming what libtest reported differs. (CLAUDE.md:
/// parameterize, don't proliferate — a sibling variant here would be the third spelling of
/// one instrument failure.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NothingRan {
    /// libtest selected no test at all: `test_count == 0`.
    NoneSelected,
    /// libtest selected tests and executed no body: `passed + failed == 0` with
    /// `test_count > 0` — an `#[ignore]`, a `--bench` run, a run-time skip.
    AllSkipped,
}

impl std::fmt::Display for NothingRan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoneSelected => "[target].filter selected ZERO tests",
            Self::AllSkipped => {
                "every test [target].filter selected was SKIPPED — not one test body executed"
            }
        })
    }
}

/// A tool whose version produced a number in the block. Recorded iff it produced one:
/// `rustc` always (every verdict comes through libtest's nightly JSON surface), `ast-grep`
/// only when the manifest declares at least one projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Instrument {
    Toolchain,
    AstGrep,
}

impl Instrument {
    /// The name as it appears both on the command line and on the block's `instrument` line.
    pub fn tool(&self) -> &'static str {
        match self {
            Self::Toolchain => "rustc",
            Self::AstGrep => "ast-grep",
        }
    }
}

impl std::fmt::Display for Instrument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tool())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InstrumentChange {
    pub instrument: Instrument,
    pub was: String,
    pub now: String,
}

/// `<tool> <was> -> <now>`, joined — used by both the `check` report and the write refusal.
pub fn describe_changes(changed: &[InstrumentChange]) -> String {
    changed
        .iter()
        .map(|c| format!("{} {} -> {}", c.instrument, c.was, c.now))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftCause {
    Code,
    Instrument(Vec<InstrumentChange>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Clean,
    Drift {
        cause: DriftCause,
        diff: String,
        numbers_moved: bool,
    },
}

/// Every condition under which probe-pin refuses to report a verdict. An aborted run writes
/// nothing: `Abort` is never rendered into a block.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum Abort {
    #[error("probe-pin: [target].mode = \"{mode}\" is not supported in schema version 1. Only \"runtime-read\" ships today: the mutant is bind-mounted and read as TEXT, never compiled. Compiled mode is deferred, not removed — see docs/probe-pin.md \"Compiled mode (deferred)\". Aborting.")]
    UnsupportedMode { mode: RunMode },

    #[error("probe-pin: this manifest does not declare exactly one control probe. The control is the [[probe]] with zero mutations: it runs the unmodified tree, and it is the only thing that shows the instrument itself works — without exactly one, a green run has no baseline and every verdict in it is unfalsifiable. Declare a single zero-mutation probe (conventionally `P0_control`, expect.outcome = \"pass\"), or delete the extra ones. Aborting.")]
    ControlMissing,

    /// `field` is the manifest key that CONTRIBUTED the token, from `Target::manifest_argv` —
    /// the same enumeration `isolate::argv` builds the argv from. It is carried rather than
    /// hard-coded because `[target].filter` reaches the argv too, FIRST, where libtest's
    /// getopts parses a `-`-leading token as a flag.
    #[error("probe-pin: {field} contributes \"{arg}\" to the target's argv. probe-pin owns --format, -Z, --nocapture, --show-output, --test-threads, --quiet and --exact: it runs the target with libtest's JSON record format, and --nocapture puts raw test output on stdout, which corrupts that stream. Remove it. Aborting.")]
    ReservedArg { field: String, arg: String },

    #[error("probe-pin: {probe} anchor \"{anchor}\" embeds a source line number. Line numbers move on every unrelated edit above them, so this anchor would rot without the pinned behaviour changing. Re-anchor on the assertion's text. Aborting.")]
    AnchorEmbedsLineNumber { probe: String, anchor: String },

    #[error("probe-pin: {probe}'s anchor list fully matches {collides_with}'s captured failure. These two probes are not distinguished — the pin would stay green if {probe}'s mutation stopped firing. Either merge {probe} and {collides_with} into one probe, or make the target's assertion message distinguish them. (Adding a unique anchor may be impossible if the only distinguishing text is a line number.) Aborting.")]
    AnchorNotDiscriminating {
        probe: String,
        collides_with: String,
    },

    #[error("probe-pin: {probe} mutation[{index}] {}: 'find' matched {found} times, expected exactly 1. 0 matches = a rotted probe; 2+ = an ambiguous mutation. No verdict in this run is valid. Aborting.", file.display())]
    FindCount {
        probe: String,
        index: usize,
        file: PathBuf,
        found: usize,
    },

    #[error("probe-pin: {probe} {}: the materialized mutant is byte-identical to the original. A no-op mutation makes the mount-reach readback meaningless — it succeeds whether or not the mount happened. Aborting.", file.display())]
    NoOpMutation { probe: String, file: PathBuf },

    #[error("probe-pin: {probe} assert_count: expected {expected} occurrence(s) of \"{text}\" in the MUTANT of {}, found {found}. The probe's headline claim is not what the mutant text says. Aborting.", file.display())]
    AssertCount {
        probe: String,
        file: PathBuf,
        text: String,
        expected: usize,
        found: usize,
    },

    #[error("probe-pin: scratch dir {} is inside the workspace {}. probe-pin must never create files under the tree it is measuring. Set TMPDIR outside the worktree.", scratch.display(), workspace.display())]
    ScratchInsideWorkspace {
        scratch: PathBuf,
        workspace: PathBuf,
    },

    #[error("probe-pin: 'cargo locate-project --workspace' failed: {stderr}. Run probe-pin from inside the workspace. Aborting.")]
    WorkspaceRootUnresolved { stderr: String },

    // The head names the PATH, not "the mutant": `isolate::scratch_dir` raises this for the
    // TMPDIR itself, where no mutant exists yet.
    #[error("probe-pin: scratch-directory write failed at {}: {err}. Mutants are written into probe-pin's scratch directory OUTSIDE the workspace, so this is a scratch-directory failure (permissions, no space, or a TMPDIR that vanished mid-run) — not a problem with your manifest, and not a write to your tree. No mutation was applied, so no verdict in this run is valid. Free space or set TMPDIR to a writable directory outside the worktree, then re-run. Aborting.", path.display())]
    MaterializeFailed { path: PathBuf, err: String },

    #[error("probe-pin: 'cargo test -p {package} --test {test} --no-run' failed: {stderr}")]
    TargetBuildFailed {
        package: String,
        test: String,
        stderr: String,
    },

    /// `found` is the number of artifacts that MATCHED (`kind == ["test"]` and the right name),
    /// which is the count the exactly-one assertion failed on. `candidates` is every artifact
    /// carrying an `executable` — a superset, and never the reported number.
    #[error("probe-pin: expected exactly one test binary for -p {package} --test {test}, found {found}. Candidates: {candidates:?}")]
    TargetBinaryUnresolved {
        package: String,
        test: String,
        found: usize,
        candidates: Vec<String>,
    },

    /// `rc: None` is "the process never started", which has no exit status at all — rendering a
    /// sentinel like `-1` there would present an invented number as a measurement.
    #[error("probe-pin: 'unshare --map-root-user --mount' {}: {stderr}. probe-pin cannot isolate mutations and will NOT fall back to writing your worktree. See docs/probe-pin.md.", match rc { Some(rc) => format!("failed (exit {rc})"), None => "could not be spawned".to_string() })]
    IsolationUnavailable { rc: Option<i32>, stderr: String },

    /// The other half of `MountNotReached`'s reserved-code pair. Everything the isolation
    /// script does before `exec` — resolving `mount`, `cmp` and `timeout` through `PATH`,
    /// expanding the `PP_*` variables, the `exec` itself — fails with EMPTY stdout and a
    /// non-zero code, which is byte-for-byte the shape of a target that died before its first
    /// libtest record. Measured: with `timeout` off `PATH` the child exits 127 with
    /// `exec: timeout: not found` on stderr, and the classification that fired was
    /// `HarnessIncomplete`, whose message tells the operator to raise `RUST_MIN_STACK`.
    #[error("probe-pin: {probe}: the isolation script FAILED before the target ran (exit {rc}). This is an instrument failure, not a probe verdict, and it is not a stack-size problem: the script resolves `mount`, `cmp` and `timeout` through PATH under `set -eu` inside `unshare --map-root-user --mount`, so a missing or non-executable one of those, an unset PP_* variable, or a `mount --bind` onto a path that does not exist all land here. Captured stderr: {stderr}. Aborting.")]
    IsolationScriptFailed {
        probe: String,
        rc: i32,
        stderr: String,
    },

    #[error("probe-pin: {probe} MOUNT NOT REACHED: {}. The mutant was materialized but the target inside the namespace still saw the ORIGINAL bytes. Every 'pass' verdict in this run would be a lie about an unmounted file. Captured stderr: {stderr}. Aborting.", path.display())]
    MountNotReached {
        probe: String,
        path: PathBuf,
        stderr: String,
    },

    #[error("probe-pin: {probe} exceeded [target].timeout_secs = {secs} and was killed. This is an instrument failure, not a probe verdict. Captured stderr: {stderr}. Raise timeout_secs, or fix the target. Aborting.")]
    TargetTimedOut {
        probe: String,
        secs: u64,
        stderr: String,
    },

    #[error("probe-pin: {probe} produced a line on stdout that is not a libtest JSON record: \"{line}\". probe-pin runs the target with --format json -Z unstable-options and requires a pure record stream. Aborting.")]
    HarnessOutputUnparseable { probe: String, line: String },

    #[error("probe-pin: {probe} {missing} (exit {rc}). This is an instrument failure, not a probe verdict. If this is a stack overflow: probe-pin runs the target with a constructed environment, and passes RUST_MIN_STACK=16777216 by default — raise it via [target].env.")]
    HarnessIncomplete {
        probe: String,
        missing: HarnessMarker,
        rc: i32,
    },

    #[error("probe-pin: the control probe '{probe}' (no mounts, unmodified tree) FAILED with exit {rc}. The instrument is broken; every other verdict in this run is meaningless. Aborting.")]
    ControlFailed { probe: String, rc: i32 },

    /// The EXECUTION floor: `passed + failed == 0`. It is written on the fields that say what
    /// RAN, because `test_count` only says what was selected — an `#[ignore]`d pinned test and
    /// a `--bench` run both report `test_count: 1` with no body executed, and both rendered a
    /// green row. This one condition covers every flag or attribute that suppresses execution,
    /// including the ones nobody enumerated.
    #[error("probe-pin: {probe} MEASURED NOTHING — {reason}: libtest reported test_count {test_count}, passed 0, failed 0, ignored {ignored}, {filtered_out} filtered out, exit {rc} (filter = \"{filter}\", filter_match = \"{filter_match}\"). A run that executed no test body is an INSTRUMENT FAILURE, not a pass — every verdict in it would be about tests that never ran. Check the filter against the target's test names, set filter_match = \"substring\", remove a selecting flag from [target].args (--skip, --ignored, --exclude-should-panic and --bench each measured driving the executed count to 0), or remove #[ignore] from the pinned test. Aborting.")]
    NothingExecuted {
        probe: String,
        reason: NothingRan,
        filter: String,
        filter_match: FilterMatch,
        test_count: usize,
        ignored: usize,
        filtered_out: usize,
        rc: i32,
    },

    #[error("probe-pin: TREE MUTATED. {} sha256 before={before} after={after}. probe-pin must never write the files it mutates.", file.display())]
    TreeMutated {
        file: PathBuf,
        before: String,
        after: String,
    },

    #[error("probe-pin: expected exactly one PROBE-PIN:BEGIN/END pair in {}, found {begins}/{ends}.", file.display())]
    MarkerNotUnique {
        file: PathBuf,
        begins: usize,
        ends: usize,
    },

    #[error("probe-pin: this manifest declares a projection but '{tool}' could not be run: {stderr}. probe-pin will not emit a block with its projection sentences missing — a table that looks complete and is unmeasured is exactly what this tool exists to prevent. Install ast-grep, set PROBE_PIN_ASTGREP, or remove the [[projection]] section. Aborting.")]
    ProjectionToolMissing { tool: String, stderr: String },

    #[error("probe-pin: '{instrument} --version' could not be run: {stderr}. probe-pin records the toolchain in the block because libtest's JSON record format is nightly-only surface. Aborting.")]
    InstrumentUnavailable {
        instrument: Instrument,
        stderr: String,
    },

    #[error("probe-pin: REFUSING to write. {} instrument(s) moved ({}) AND a measured number moved ({deltas}). Re-stamping would record the new number under the new tool and destroy the evidence of which one changed it. Pin one version and re-run, or — if you have confirmed the change is real — delete the PROBE-PIN:BEGIN…END block and re-run to write a fresh one. Aborting.", changed.len(), describe_changes(changed))]
    WriteUnderInstrumentChange {
        changed: Vec<InstrumentChange>,
        deltas: String,
    },
}

/// sha256, lowercase hex — the one hashing entry point (fingerprints and the block digest).
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
