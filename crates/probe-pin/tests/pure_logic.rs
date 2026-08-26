//! Tier 1 — §10 rows 3-13, 15-20, 22-31 and 33's non-ordering arms. Zero namespace
//! dependency: nothing here runs `unshare`.
//!
//! Zero namespace dependency is not the same as host-portable. The arms that drive a real
//! libtest child spawn GNU `timeout(1)` — the same binary the shipping isolation script
//! resolves through `PATH` — which macOS does not ship. Those arms are `ignore`d off Linux
//! rather than `cfg`'d out, on purpose: a compiled-out test is INVISIBLE in the report, and
//! an unmeasured claim that reports as nothing is the same failure as one that reports as a
//! pass. An ignored test reports as ignored — the vocabulary the Tier-2 `#[ignore]`s already
//! use for "this venue cannot measure it".
//!
//! The `ppfixture_*` tests at the bottom are not assertions about probe-pin — they are the
//! TARGETS the Tier-2 dogfood manifests and the child-process arms below drive. They are
//! inert (or trivially passing) unless `PP_FIXTURE` selects them, so a plain
//! `cargo test --test pure_logic` runs them as no-ops.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use probe_pin::block::{self, Outcome, ProjectionCount};
use probe_pin::isolate::{self, Run};
use probe_pin::manifest::{self, Manifest, Probe, Target};
use probe_pin::mutate;
use probe_pin::project;
use probe_pin::target;
use probe_pin::verdict::{self, Verdict};
use probe_pin::{
    Abort, CheckOutcome, DriftCause, HarnessMarker, Instrument, InstrumentChange, NothingRan,
};

mod common;

// ------------------------------------------------------------------ helpers

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load(name: &str) -> Manifest {
    Manifest::load(&fixtures().join(name)).expect("fixture manifest parses")
}

fn abort_of(e: anyhow::Error) -> Abort {
    e.downcast::<Abort>().expect("error is an Abort")
}

/// A `[target]` good enough to classify a synthetic record stream against.
fn target() -> Target {
    load("dogfood.toml").target
}

fn run_of(rc: i32, stdout: &str) -> Run {
    Run {
        rc,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

/// A libtest JSON stream: suite started, the given test records, suite terminal.
fn stream(test_count: usize, records: &[&str], terminal: &str) -> String {
    let mut s =
        format!("{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":{test_count}}}\n");
    for r in records {
        s.push_str(r);
        s.push('\n');
    }
    s.push_str(&format!(
        "{{\"type\":\"suite\",\"event\":\"{terminal}\",\"passed\":0,\"failed\":1,\"filtered_out\":3}}\n"
    ));
    s
}

fn probe(toml: &str) -> Probe {
    toml::from_str(toml).expect("probe fixture parses")
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Re-exec THIS test binary as a libtest target: the cheapest real record stream there is.
/// `filter` is a SUBSTRING filter, exactly like the shipping argv; `extra` is where a test
/// adds `--exact` or a `[target].args` flag.
fn child(filter: &str, env: &[(&str, &str)], extra: &[&str]) -> Run {
    // Under `timeout`, exactly as the shipping script runs the target. That is not cosmetic:
    // a signal-killed child has no exit CODE at all, and `timeout` is what normalizes SIGABRT
    // to the 134 the pipeline classifies on.
    let mut cmd = Command::new("timeout");
    cmd.args(["-k", "5", "60"])
        .arg(std::env::current_exe().unwrap())
        .args([filter, "--test-threads=1", "--format", "json"])
        .args(["-Z", "unstable-options"])
        .args(extra);
    cmd.env_clear();
    for key in ["PATH", "HOME"] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("child target runs");
    Run {
        rc: isolate::exit_rc(&out.status),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ------------------------------------------------------------------ row 3, 4, 5, 6

/// Row 3: anchors are scoped to ONE `{"type":"test"}` `failed` record — not to the whole
/// stdout, and not to an inferred prose block. DROP (scan the whole stdout) makes the forged
/// anchor hit; TRIVIALIZE (one buffer) makes the cross-record pair hit.
#[test]
fn json_scope() {
    let s = stream(
        3,
        &[
            r#"{"type":"test","name":"c_pass","event":"ok","stdout":"FORGED FROM A PASSING TEST\n"}"#,
            r#"{"type":"test","name":"a_fail","event":"failed","stdout":"REAL ANCHOR ONE\nREAL ANCHOR TWO\n"}"#,
            r#"{"type":"test","name":"b_fail","event":"failed","stdout":"OTHER FAILURE\n"}"#,
        ],
        "failed",
    );
    let obs =
        verdict::observe("P1", &run_of(101, &s), &target(), &[]).expect("no instrument failure");
    let anchors = |v: &[&str]| {
        verdict::all_anchors_in_one(
            &v.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &obs.failed_captures,
        )
    };

    // hostile: a passing test's own output can never satisfy an anchor
    assert!(!anchors(&["FORGED FROM A PASSING TEST"]));
    // hostile: two anchors that only jointly exist ACROSS two failed records
    assert!(!anchors(&["REAL ANCHOR ONE", "OTHER FAILURE"]));
    // reach guard: the real two-fragment anchor, both fragments in ONE record
    assert!(anchors(&["REAL ANCHOR ONE", "REAL ANCHOR TWO"]));
    assert_eq!(obs.failed_captures.len(), 2);
}

/// Row 4: a `#[should_panic]` test that PASSES is not a failure. libtest reports it
/// `event:"ok"` with no capture, so it is unreachable by any anchor.
#[test]
fn should_panic() {
    // The `event:"started"` records are what a REAL libtest stream carries, and they are what
    // separates "failed" from the trivialized "every record whose event != ok".
    let s = stream(
        2,
        &[
            r#"{"type":"test","event":"started","name":"d_should_panic_passes"}"#,
            r#"{"type":"test","name":"d_should_panic_passes","event":"ok"}"#,
            r#"{"type":"test","event":"started","name":"a_fail"}"#,
            r#"{"type":"test","name":"a_fail","event":"failed","stdout":"REAL\n"}"#,
        ],
        "failed",
    );
    let obs = verdict::observe("P1", &run_of(101, &s), &target(), &[]).unwrap();
    assert_eq!(obs.failed_captures, vec!["REAL\n".to_string()]);
    assert!(!obs
        .failed_captures
        .iter()
        .any(|c| c.contains("should_panic")));
}

/// Row 5: a panic whose MESSAGE contains a `thread '…' panicked at` line stays ONE record.
/// The deleted prose block-splitter would have cut it in two and produced a false refutation.
#[test]
fn nested_thread_line() {
    let inner = "thread 'inner' (1) panicked at src/x.rs:1:1:\\nFRAGMENT TWO";
    let s = stream(
        1,
        &[&format!(
            r#"{{"type":"test","name":"e","event":"failed","stdout":"\nthread 'e' (2) panicked at tests/mount.rs:7:5:\nFRAGMENT ONE {inner}\n"}}"#
        )],
        "failed",
    );
    let obs = verdict::observe("P1", &run_of(101, &s), &target(), &[]).unwrap();
    assert_eq!(obs.failed_captures.len(), 1);
    assert!(verdict::all_anchors_in_one(
        &["FRAGMENT ONE".to_string(), "FRAGMENT TWO".to_string()],
        &obs.failed_captures
    ));
}

/// Row 6: JSON escaping round-trips exactly — quote, backslash, tab, ESC, non-ASCII.
#[test]
fn escaping() {
    let s = stream(
        1,
        &[r#"{"type":"test","name":"f","event":"failed","stdout":"q=\" b=\\ t=\t e=\u001b n=é"}"#],
        "failed",
    );
    let obs = verdict::observe("P1", &run_of(101, &s), &target(), &[]).unwrap();
    assert_eq!(obs.failed_captures[0], "q=\" b=\\ t=\t e=\u{1b} n=é");
}

// ------------------------------------------------------------------ row 7

/// Row 7: every reserved flag is rejected, on the option NAME rather than the raw token —
/// `--test-threads=4`, `--format=terse` and `-Zunstable-options` are the three spellings a
/// raw-token list misclassifies. The four negative controls still validate, so the fix does
/// not narrow the live `args` field.
#[test]
fn reserved_arg() {
    let mut m = load("dogfood.toml");
    for hostile in [
        "--nocapture",
        "--format",
        "--test-threads=4",
        "--format=terse",
        "-Zunstable-options",
        "--exact",
        "--show-output",
        "-q",
    ] {
        m.target.args = vec![hostile.to_string()];
        let e = abort_of(manifest::validate(&m).unwrap_err());
        assert!(
            matches!(e, Abort::ReservedArg { ref arg, ref field } if arg == hostile && field == "[target].args"),
            "{hostile}"
        );
    }
    for benign in [
        "--skip=foo",
        "--skip",
        "--ignored",
        "--exclude-should-panic",
    ] {
        m.target.args = vec![benign.to_string()];
        assert!(manifest::validate(&m).is_ok(), "{benign}");
    }
    m.target.args = vec![];
    assert!(manifest::validate(&m).is_ok());

    // Why they are reserved: ONE non-JSON line on stdout breaks the strict parse.
    let mut s = stream(1, &[], "ok");
    s.insert_str(0, "SECOND-LINE-OF-B: got [Site { line: 11492 }]\n");
    let e = verdict::observe("P1", &run_of(0, &s), &target(), &[]).unwrap_err();
    assert!(matches!(e, Abort::HarnessOutputUnparseable { .. }));
}

// ------------------------------------------------------------------ row 8

/// Row 8: the harness must have COMPLETED. Fixture A aborts unconditionally, so no
/// `RUST_MIN_STACK` value rescues it — the suite terminal record is what fires, and dropping
/// the marker requirement would turn rc 134 into a `Verdict::Fail`.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "spawns GNU timeout(1); see docs/probe-pin.md"
)]
fn harness() {
    for stack in ["", "1", "16777216", "268435456"] {
        let env: Vec<(&str, &str)> = if stack.is_empty() {
            vec![("PP_FIXTURE", "abort")]
        } else {
            vec![("PP_FIXTURE", "abort"), ("RUST_MIN_STACK", stack)]
        };
        let run = child("ppfixture_abort", &env, &[]);
        assert_eq!(
            run.rc, 134,
            "RUST_MIN_STACK={stack:?} should not rescue an abort()"
        );
        let e = verdict::observe("P1", &run, &target(), &[]).unwrap_err();
        assert!(
            matches!(
                e,
                Abort::HarnessIncomplete {
                    missing: HarnessMarker::SuiteFinished,
                    rc: 134,
                    ..
                }
            ),
            "RUST_MIN_STACK={stack:?} -> {e}"
        );
    }
    // reach guard: the same target, not aborting, classifies cleanly
    let run = child("ppfixture_abort", &[], &[]);
    assert_eq!(run.rc, 0);
    assert!(verdict::observe("P1", &run, &target(), &[]).is_ok());
}

/// Row 8's neighbour, and the classification it was swallowing. The isolation script resolves
/// `mount`, `cmp` and `timeout` through `PATH` under `set -eu` — and a shell that dies before
/// `exec` produces EMPTY stdout with a non-zero code, which is byte-for-byte the shape of a
/// target that died before its first record. Measured on the shipping script with `timeout` off
/// `PATH`: exit 127, `probe-pin: line 10: exec: timeout: not found` on stderr, no reserved code
/// fired, and the abort raised was `HarnessIncomplete { SuiteStarted }` — whose message tells
/// the operator to raise `RUST_MIN_STACK` through `[target].env`, which cannot make `timeout`
/// exist. The script now names the cause with reserved code 96.
///
/// The real script raising 96 is `isolation_e2e::script_failure_is_named_not_inferred`; this is
/// the classification half, where the DROP arm (the reserved code absent, i.e. exactly the
/// measurement above) is a literal `Run`.
#[test]
fn script_failure_is_named_not_inferred() {
    let t = target();
    let script_failed = Run {
        rc: 96,
        stdout: String::new(),
        stderr: "probe-pin: ISOLATION SCRIPT FAILED before the target ran".to_string(),
    };
    let e = verdict::observe("P1", &script_failed, &t, &[]).unwrap_err();
    assert!(
        matches!(e, Abort::IsolationScriptFailed { rc: 96, .. }),
        "{e}"
    );
    assert!(e.to_string().contains("PATH"), "{e}");
    assert!(
        !e.to_string().contains("RUST_MIN_STACK"),
        "the remedy must not name a stack size: {e}"
    );

    // DROP — the same failure WITHOUT the reserved code, which is what was measured before it
    // existed. It classifies as the harness dying mid-run, and names the wrong remedy.
    let unreserved = Run {
        rc: 127,
        stdout: String::new(),
        stderr: "probe-pin: line 10: exec: timeout: not found".to_string(),
    };
    let e = verdict::observe("P1", &unreserved, &t, &[]).unwrap_err();
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

    // and 96 is trusted only with EMPTY stdout, exactly like 97: a target that emitted records
    // reached `exec`, so the code is the target's own
    let with_records = Run {
        rc: 96,
        stdout: stream(1, &[], "ok"),
        stderr: String::new(),
    };
    assert!(verdict::observe("P1", &with_records, &t, &[]).is_ok());
}

// ------------------------------------------------------------------ row 9

/// Row 9: no anchor may embed a source line number. Measured on the real corpus: 0 of 10
/// shipping anchors rejected, 7 of 9 hostile spellings rejected, and 0 of 8 plausible
/// legitimate anchors whose text merely ENDS in "line" rejected.
#[test]
fn anchor_lint() {
    let shipping = [
        "PROVENANCE SPLIT VIOLATED",
        "text: \"Ok(TargetPin::Player(*player))\"",
        "text: \"TargetRef::Player(pl) => Some(TargetPin::Player(*pl)),\"",
        "site 2 must remain `shortcut_drive_period`'s MATCH arm",
        "got \"TargetPin::Player(_) => 1,\"",
        "site 3 must remain the CR 701.34a proliferate CONSTRUCTION",
        "got \"Some(crate::analysis::decision_template::TargetPin::Player(*other_seat))\"",
        "got \"Some(crate::analysis::decision_template::TargetPin::Player(*pl))\"",
        "the TARGET-class spelling must be CONSTRUCTED by BOTH producers",
        "(\"engine/src/game/engine.rs\", 1)",
    ];
    let rejected = |xs: &[&str]| {
        xs.iter()
            .filter(|a| manifest::embeds_line_number(a))
            .count()
    };
    assert_eq!(
        rejected(&shipping),
        0,
        "a shipping anchor must never be refused"
    );

    let hostile = [
        "line: 8945",
        "line: 4689",
        "line: 11492",
        "loop_shortcut_seat_pin_census.rs:160:5",
        "got [Site { file: \"engine/src/game/engine.rs\", line: 11490 }]",
        "at line 11492",
        "engine.rs:11492",
        "L11492",                                 // named residual (§12.D row 3)
        "(\"engine/src/game/engine.rs\", 11492)", // named residual: P9's shape, up to the integer
    ];
    assert_eq!(rejected(&hostile), 7);
    assert!(!manifest::embeds_line_number("L11492"));
    assert!(!manifest::embeds_line_number(
        "(\"engine/src/game/engine.rs\", 11492)"
    ));

    // MINOR-E: `\bline`. Words ENDING in "line" followed by a count are legitimate, and this
    // repo's own assertion strings contain them.
    let legit = [
        "baseline: 1",
        "baseline: 0",
        "baseline 3 producers must construct the pin",
        "inline 2 callers remain",
        "the pipeline 4 stages are pinned",
        "deadline: 5 seconds",
        "airline 7",
        "outline: 3 sections",
    ];
    assert_eq!(rejected(&legit), 0);

    // and the lint is wired into validate()
    let mut m = load("dogfood.toml");
    m.probes[1].expect = toml::from_str("outcome='fail'\nanchor=['line: 4689']").unwrap();
    assert!(matches!(
        abort_of(manifest::validate(&m).unwrap_err()),
        Abort::AnchorEmbedsLineNumber { .. }
    ));
}

// ------------------------------------------------------------------ row 10

/// Row 10: all-pairs discrimination over the six archived failing probes. Self-misses 0 is
/// the reach guard; collisions 0 is the claim; r2's one-entry P1 anchor is the hostile
/// sibling that MUST abort, because it also fully matches P2's capture.
#[test]
fn all_pairs() {
    let corpus: [(&str, [&str; 2]); 6] = [
        (
            "P1",
            [
                "PROVENANCE SPLIT VIOLATED",
                "text: \"Ok(TargetPin::Player(*player))\"",
            ],
        ),
        (
            "P2",
            [
                "PROVENANCE SPLIT VIOLATED",
                "text: \"TargetRef::Player(pl) => Some(TargetPin::Player(*pl)),\"",
            ],
        ),
        (
            "P6a",
            [
                "site 2 must remain `shortcut_drive_period`'s MATCH arm",
                "got \"TargetPin::Player(_) => 1,\"",
            ],
        ),
        (
            "P6b",
            [
                "site 3 must remain the CR 701.34a proliferate CONSTRUCTION",
                "got \"Some(crate::analysis::decision_template::TargetPin::Player(*other_seat))\"",
            ],
        ),
        (
            "P7",
            [
                "site 2 must remain `shortcut_drive_period`'s MATCH arm",
                "got \"Some(crate::analysis::decision_template::TargetPin::Player(*pl))\"",
            ],
        ),
        (
            "P9",
            [
                "the TARGET-class spelling must be CONSTRUCTED by BOTH producers",
                "(\"engine/src/game/engine.rs\", 1)",
            ],
        ),
    ];
    let mut probes = Vec::new();
    let mut captures: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, anchors) in &corpus {
        probes.push(probe(&format!(
            "id = {id:?}\n[[mutation]]\nkind='replace'\nfile='f'\nfind='a'\nreplace='b'\n[expect]\noutcome='fail'\nanchor={:?}\n",
            anchors
        )));
        captures.insert(
            (*id).to_string(),
            vec![format!(
                "panicked at tests/census.rs:1:1:\n{}\n{}\n",
                anchors[0], anchors[1]
            )],
        );
    }

    // reach guard: every probe's own anchors hit its own capture
    for p in &probes {
        assert!(
            verdict::all_anchors_in_one(p.anchors(), &captures[&p.id]),
            "self-miss on {}",
            p.id
        );
    }
    assert_eq!(verdict::discriminate(&probes, &captures), Ok(()));

    // hostile: r2's one-entry P1 anchor list also matches P2's capture
    let mut hostile = probes.clone();
    hostile[0].expect =
        toml::from_str("outcome='fail'\nanchor=['PROVENANCE SPLIT VIOLATED']").unwrap();
    assert!(matches!(
        verdict::discriminate(&hostile, &captures),
        Err(Abort::AnchorNotDiscriminating { ref probe, ref collides_with }) if probe == "P1" && collides_with == "P2"
    ));
}

// ------------------------------------------------------------------ row 11

/// Row 11: byte-identical anchor lists are caught at VALIDATE time — before any run. The
/// TRIVIALIZE (compare lengths only) would accept two same-length distinct lists.
#[test]
fn anchor_dup() {
    let mut m = load("dogfood.toml");
    let dup: probe_pin::manifest::Expect =
        toml::from_str("outcome='fail'\nanchor=['A','B']").unwrap();
    m.probes[1].expect = dup.clone();
    m.probes[2].expect = dup;
    assert!(matches!(
        abort_of(manifest::validate(&m).unwrap_err()),
        Abort::AnchorNotDiscriminating { .. }
    ));
    // positive: two distinct lists of the SAME LENGTH validate
    m.probes[2].expect = toml::from_str("outcome='fail'\nanchor=['A','C']").unwrap();
    assert!(manifest::validate(&m).is_ok());
}

// ------------------------------------------------------------------ row 12

/// Row 12: the target's environment is CONSTRUCTED, not inherited.
///
/// The claim is about what a PROCESS's ambient environment does, so the measurement needs a
/// process whose ambient environment the test owns — mutating this one would race its parallel
/// siblings. `ppfixture_envprobe` is that process: it calls the shipping `apply_child_env` and
/// writes the resulting capture length to `PP_OUT`. Both a DROP (no clear) and a TRIVIALIZE
/// (clear, then re-import everything) leak the ambient value into the innermost target.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "spawns GNU timeout(1); see docs/probe-pin.md"
)]
fn env_hermetic() {
    let dir = tempfile::tempdir().unwrap();
    let measure = |name: &str, ambient: &[(&str, &str)]| -> usize {
        let out = dir.path().join(name);
        let out_s = out.to_str().unwrap().to_string();
        let mut env: Vec<(&str, &str)> = vec![("PP_FIXTURE", "envprobe"), ("PP_OUT", &out_s)];
        env.extend_from_slice(ambient);
        let run = child("ppfixture_envprobe", &env, &[]);
        assert_eq!(run.rc, 0, "envprobe: {}", run.stdout);
        std::fs::read_to_string(&out)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    };

    let clean = measure("clean", &[]);
    assert!(
        clean > 0,
        "the reach guard: a panic capture must be non-empty"
    );

    // an exported RUST_BACKTRACE is the developer-shell hazard: it changes the captured bytes,
    // and the CLEAR is the only thing stopping it — `apply_child_env` deliberately does not
    // re-set it to "0", which would make this arm pass with or without the clear (MINOR-5)
    assert_eq!(
        measure("backtrace", &[("RUST_BACKTRACE", "1")]),
        clean,
        "an ambient RUST_BACKTRACE must not reach the target"
    );
    // and RUST_TEST_NOCAPTURE is the severe one: libtest then drops the `stdout` field from
    // every failed record, so every anchor in every probe silently stops matching
    assert_eq!(
        measure("nocapture", &[("RUST_TEST_NOCAPTURE", "1")]),
        clean,
        "an ambient RUST_TEST_NOCAPTURE must not reach the target"
    );
    // the ALLOWLIST, not the clear, is what decides: [target].env turns it back on
    assert!(
        measure("forced", &[("PP_FORCE_BACKTRACE", "1")]) > clean,
        "[target].env must still be able to set RUST_BACKTRACE"
    );
}

// ------------------------------------------------------------------ row 13

/// Row 13: `[target].env` defaults to `RUST_MIN_STACK = "16777216"`, and that value is
/// load-bearing. Fixture B is RESCUABLE — unlike row 8's fixture, which no value rescues —
/// so `"1"` (the TRIVIALIZE: a default that exists and is useless) is measurably different
/// from the shipping default.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "spawns GNU timeout(1); see docs/probe-pin.md"
)]
fn env_default() {
    let t: Target = toml::from_str(
        "mode='runtime-read'\npackage='p'\ntest='t'\nfilter='f'\nfilter_match='substring'",
    )
    .unwrap();
    assert_eq!(
        t.env.get("RUST_MIN_STACK").map(String::as_str),
        Some("16777216")
    );
    assert_eq!(t.timeout_secs, 300);
    assert!(t.args.is_empty());

    let rc = |stack: Option<&str>| {
        let env: Vec<(&str, &str)> = match stack {
            Some(v) => vec![("PP_FIXTURE", "stack"), ("RUST_MIN_STACK", v)],
            None => vec![("PP_FIXTURE", "stack")],
        };
        child("ppfixture_stack", &env, &[]).rc
    };
    assert_eq!(rc(None), 134, "unset must overflow");
    assert_eq!(rc(Some("1")), 134, "a useless default must overflow");
    assert_eq!(
        rc(Some("16777216")),
        0,
        "the shipping default must rescue it"
    );
}

/// Schema adequacy (§6 / §11 gate 6), MEASURED rather than asserted: every shipped manifest
/// parses under `deny_unknown_fields` and passes validation. `p7.toml` is the archived P7
/// probe verbatim in shape — two sequential `Replace`s on one file whose second `find` exists
/// only in the first's output, two `assert_count`s, a two-entry line-free anchor — and `p8`
/// is the 5-file × 200 `Prepend`. Nothing in the corpus was reshaped to fit the schema.
#[test]
fn schema_adequacy() {
    for name in [
        "collide.toml",
        "control_expect_fail.toml",
        "control_fails.toml",
        "dogfood.toml",
        "p7.toml",
        "p8.toml",
        "projection.toml",
        "treewrite.toml",
        "unsorted_ids.toml",
    ] {
        let m = load(name);
        manifest::validate(&m).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let p7 = load("p7.toml");
    let seq = &p7.probes[1];
    assert_eq!(seq.mutations.len(), 2);
    assert_eq!(seq.touched().len(), 1, "both ops target ONE file");
    assert_eq!(seq.assert_counts.len(), 1);
    assert_eq!(seq.anchors().len(), 2);
    match &load("p8.toml").probes[1].mutations[0] {
        probe_pin::manifest::Mutation::Prepend { files, repeat, .. } => {
            assert_eq!((files.len(), *repeat), (5, 200));
        }
        m => panic!("P8 must be a Prepend: {m:?}"),
    }
}

// ------------------------------------------------------------------ row 15

/// Row 15: `mode = "compiled"` PARSES and is rejected, visibly, with a deferral message.
#[test]
fn compiled_rejected() {
    let m = load("compiled.toml");
    let e = abort_of(manifest::validate(&m).unwrap_err());
    assert!(matches!(e, Abort::UnsupportedMode { .. }));
    assert!(e.to_string().contains("deferred, not removed"));

    // positive: runtime-read validates
    assert!(manifest::validate(&load("dogfood.toml")).is_ok());
    // hostile: a TYPO is a different failure — an unknown variant, at parse time
    let e = toml::from_str::<Manifest>(
        &std::fs::read_to_string(fixtures().join("compiled.toml"))
            .unwrap()
            .replace("\"compiled\"", "\"cmpiled\""),
    )
    .unwrap_err();
    assert!(e.to_string().contains("unknown variant"), "{e}");
}

// ------------------------------------------------------------------ row 16

/// Row 16: a `--no-run --message-format=json` stream carries MORE than one `executable` —
/// the package's `bin` reports one too, and it comes FIRST. Taking the first picks the bin.
#[test]
fn resolve_bin() {
    let s = concat!(
        r#"{"reason":"compiler-artifact","executable":"/t/debug/r4probe","target":{"kind":["bin"],"name":"r4probe"}}"#,
        "\n",
        r#"{"reason":"compiler-artifact","executable":"/t/debug/deps/blocks-47ca","target":{"kind":["test"],"name":"blocks"}}"#,
        "\n",
        r#"{"reason":"build-finished","success":true}"#,
    );
    assert_eq!(
        target::pick_executable(s, "p", "blocks").unwrap(),
        PathBuf::from("/t/debug/deps/blocks-47ca")
    );
    // hostile: zero test artifacts -> named candidates, never a silent first-match. The
    // REPORTED count is the number that MATCHED, not the number of candidates (MINOR-1): a
    // user with a typo'd [target].test was being told two test binaries were found and sent
    // hunting for a duplicate that does not exist.
    let e = target::pick_executable(s, "p", "absent").unwrap_err();
    assert!(
        matches!(e, Abort::TargetBinaryUnresolved { found: 0, ref candidates, .. } if candidates.len() == 2),
        "{e:?}"
    );
    assert!(e.to_string().contains("found 0. Candidates: ["), "{e}");
}

// ------------------------------------------------------------------ row 17

/// Row 17: `find` must occur EXACTLY once, against the RUNNING text — so a two-op sequence
/// whose second `find` only exists after the first op is expressible, and an ambiguous or
/// rotted `find` is refused.
#[test]
fn find_gate() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    write(dir.path(), "f.txt", "alpha\nbeta\nalpha\n");

    let mk = |ops: &str| probe(&format!("id='P'\n{ops}\n[expect]\noutcome='pass'\n"));
    let op = |find: &str, replace: &str| {
        format!("[[mutation]]\nkind='replace'\nfile='f.txt'\nfind={find:?}\nreplace={replace:?}")
    };

    let e =
        abort_of(mutate::apply(&mk(&op("gamma", "x")), dir.path(), scratch.path()).unwrap_err());
    assert!(
        matches!(
            e,
            Abort::FindCount {
                found: 0,
                index: 0,
                ..
            }
        ),
        "{e}"
    );
    let e =
        abort_of(mutate::apply(&mk(&op("alpha", "x")), dir.path(), scratch.path()).unwrap_err());
    assert!(matches!(e, Abort::FindCount { found: 2, .. }), "{e}");

    // running text: op 2's `find` exists ONLY after op 1
    let seq = format!("{}\n{}", op("beta", "delta"), op("delta", "epsilon"));
    assert!(mutate::apply(&mk(&seq), dir.path(), scratch.path()).is_ok());
    // and the same op 2 alone is a rotted probe
    let e = abort_of(
        mutate::apply(&mk(&op("delta", "epsilon")), dir.path(), scratch.path()).unwrap_err(),
    );
    assert!(matches!(e, Abort::FindCount { found: 0, .. }), "{e}");
}

// ------------------------------------------------------------------ row 18

/// Row 18: the no-op gate is per FILE, on MATERIALIZED bytes. That one comparison covers
/// `Prepend { repeat: 0 }`, `Prepend { text: "" }` and a sequence that cancels out — none of
/// which the deleted per-op `replace != find` check ever caught. Without it a mutant that
/// equals the original defeats the mount readback: the `cmp` succeeds unmounted.
#[test]
fn noop() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    write(dir.path(), "f.txt", "alpha\nbeta\n");
    let mk = |ops: &str| probe(&format!("id='P'\n{ops}\n[expect]\noutcome='pass'\n"));

    for hostile in [
        "[[mutation]]\nkind='prepend'\nfiles=['f.txt']\ntext='// pad\\n'\nrepeat=0",
        "[[mutation]]\nkind='prepend'\nfiles=['f.txt']\ntext=''\nrepeat=200",
        "[[mutation]]\nkind='replace'\nfile='f.txt'\nfind='alpha'\nreplace='gamma'\n\
         [[mutation]]\nkind='replace'\nfile='f.txt'\nfind='gamma'\nreplace='alpha'",
    ] {
        let e = abort_of(mutate::apply(&mk(hostile), dir.path(), scratch.path()).unwrap_err());
        assert!(matches!(e, Abort::NoOpMutation { .. }), "{hostile} -> {e}");
    }
    // positive: a DELETION is not a no-op
    assert!(mutate::apply(
        &mk("[[mutation]]\nkind='replace'\nfile='f.txt'\nfind='alpha'\nreplace=''"),
        dir.path(),
        scratch.path()
    )
    .is_ok());
}

/// Row 18's escape (final review-impl, MAJOR-1): a mutation whose `files` list is EMPTY never
/// reaches the no-op gate — it seeds no running file, so the per-file loop never executes, no
/// mount is built, and the probe renders a green `pass` row carrying the control's own firing
/// text for a run of the UNMODIFIED tree. The refusal is per MUTATION, so the shape cannot be
/// smuggled in beside one that does name a file either.
#[test]
fn mutation_must_name_a_file() {
    let text = std::fs::read_to_string(fixtures().join("dogfood.toml")).unwrap();
    let parse = |t: &str| toml::from_str::<Manifest>(t).expect("manifest parses");
    // positive: the shipping manifest, unedited, validates
    manifest::validate(&parse(&text)).unwrap();

    let empty = text.replace(
        "  files  = [\"crates/probe-pin/tests/fixtures/prod.txt\"]",
        "  files  = []",
    );
    assert_ne!(
        empty, text,
        "the fixture edit must apply, or this arm is vacuous"
    );
    let m = parse(&empty);
    assert!(
        m.probes[2].touched().is_empty() && !m.probes[2].mutations.is_empty(),
        "the shape under test: mutations declared, no file named"
    );
    let e = manifest::validate(&m).unwrap_err();
    assert!(e.to_string().contains("names no file"), "{e}");
    assert!(e.to_string().contains("P2_pad_reaches_target"), "{e}");

    // beside a real mutation: still refused, and the index names WHICH one
    let no_file =
        "  [[probe.mutation]]\n  kind   = \"prepend\"\n  files  = []\n  text   = \"// x\\n\"\n";
    let mixed = text.replace(
        "  [[probe.mutation]]\n  kind    = \"replace\"",
        &format!("{no_file}  [[probe.mutation]]\n  kind    = \"replace\""),
    );
    assert_ne!(
        mixed, text,
        "the fixture edit must apply, or this arm is vacuous"
    );
    let e = manifest::validate(&parse(&mixed)).unwrap_err();
    assert!(e.to_string().contains("mutation[0] names no file"), "{e}");
}

/// MAJOR-1's sibling, found by sweeping the same shape (a declaration that never reaches its
/// gate): `assert_count` is evaluated against the MUTANT text inside `mutate::apply`, which the
/// control never enters. Measured against the fixed binary before this refusal existed — a
/// control declaring `count = 99` of a string that occurs nowhere ran green at exit 0, with the
/// unevaluated triple pinned into the digest.
#[test]
fn control_may_not_declare_an_assert_count() {
    let text = std::fs::read_to_string(fixtures().join("dogfood.toml")).unwrap();
    let ac = "  [[probe.assert_count]]\n  file  = \"crates/probe-pin/tests/fixtures/prod.txt\"\n  text  = \"NOWHERE\"\n  count = 99\n";
    let anchor_line = "claim = \"no mounts, unmodified tree — the instrument itself\"\n";
    let on_control = text.replace(anchor_line, &format!("{anchor_line}{ac}"));
    assert_ne!(
        on_control, text,
        "the fixture edit must apply, or this arm is vacuous"
    );
    let m = toml::from_str::<Manifest>(&on_control).expect("manifest parses");
    assert!(
        m.probes[0].mutations.is_empty() && !m.probes[0].assert_counts.is_empty(),
        "the shape under test: the control carries an assert_count"
    );
    let e = manifest::validate(&m).unwrap_err();
    assert!(
        e.to_string().contains("assert_count(s) with no mutation"),
        "{e}"
    );
    // positive: the SAME assert_count on a probe that mutates is the shipping shape (P1 already
    // carries one), so the refusal is about the missing mutant, not about assert_count itself
    assert!(manifest::validate(&toml::from_str::<Manifest>(&text).unwrap()).is_ok());
}

/// `target::resolve` must run cargo with the workspace root as its cwd, and this is asserted
/// against the SOURCE because no behavioural test can see it: with the pin removed the whole
/// Tier-2 suite still passes 15/15 — only the *location of a side effect* moves. Measured, both
/// arms, same command: pinned, nothing appears; unpinned, a 285M `crates/probe-pin/target/`
/// materializes, which no `.gitignore` rule covers because the repo ignores a root-anchored
/// `/target`.
///
/// The cause is that `CARGO_TARGET_DIR` is commonly relative (the Tilt resources pass
/// `target/probe-pin`) and a relative value resolves against the cwd of whichever process
/// finally execs cargo — here a test whose cwd is the crate directory, not the root.
/// `workspace_root` is deliberately NOT pinned: inheriting the caller's cwd is how it discovers
/// the root at all.
#[test]
fn target_resolve_pins_cargo_to_the_workspace_root() {
    let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/target.rs"))
        .expect("target.rs is readable");

    let resolve = src
        .split_once("pub fn resolve(")
        .expect("target::resolve exists")
        .1;
    let body = resolve
        .split_once("pub fn ")
        .map_or(resolve, |(before, _)| before);
    assert!(
        body.contains(".current_dir(root)"),
        "target::resolve no longer pins cargo's cwd to the workspace root. A relative \
         CARGO_TARGET_DIR then resolves against whatever cwd the caller had, scattering build \
         artifacts outside the root-anchored /target the repo ignores. The Tier-2 suite cannot \
         catch this — it passes either way."
    );

    // Instrument control: the same search must NOT find the pin in `workspace_root`, which is
    // correct precisely because it inherits the caller's cwd. If this ever matches, the search
    // is picking up the wrong function and the assertion above proves nothing.
    let wsr = src
        .split_once("pub fn workspace_root(")
        .expect("workspace_root exists")
        .1;
    let wsr_body = wsr.split_once("pub fn ").map_or(wsr, |(before, _)| before);
    assert!(
        !wsr_body.contains(".current_dir("),
        "workspace_root must NOT pin a cwd — it discovers the root FROM the cwd. If it now does, \
         this test's function-slicing is wrong and both assertions need re-reading."
    );
}

/// The `splice` postcondition is only worth what the WRITER CENSUS is worth.
///
/// `splice` refusing to return an unlocatable block makes "`run --write` cannot commit a block its
/// own next `check` aborts on" unconditional — but only while `splice` is the sole route to the
/// bytes of the pinned file. A second `fs::write` of `output.file` added later would falsify that
/// sentence in the docs, the PR body and the crate's own comment block, with nothing failing. That
/// is this crate's recurring defect class wearing new clothes: prose stays green after the thing
/// it describes has moved.
///
/// So the census is executable rather than recited. Measured at the head that added the
/// postcondition: exactly two write producers exist in `src/` — `mutate.rs` (the mutant, into the
/// scratch dir, never a rendered block) and `main.rs` (the pinned file), and the latter's content
/// argument is literally the `splice(..)?` call. The guard is not merely *on* the write path; it
/// is *inside the only write's argument*, so no byte sequence reaches the file without it.
///
/// COUNTED ON SYNTAX, NOT TEXT — but stated as what it is, because the first draft of this
/// comment got it wrong in the direction that flatters the code. It claimed the comment-strip was
/// load-bearing today, citing `manifest.rs`'s prose mention of `fs::write`. Measured: that mention
/// is in backticks with no open paren, every producer pattern here requires the paren, and
/// deleting the strip left this test GREEN — the arm did not flip. The strip is DEFENSIVE, not
/// currently load-bearing.
///
/// Its value is therefore measured against the input it exists for rather than asserted: inserting
/// a doc comment that quotes a call site (`// e.g. std::fs::write(&dest, mutant) ...`) into
/// `mutate.rs` keeps this test green WITH the strip and fails it at `found 3` WITHOUT — same
/// source, both arms. So it earns its three lines against a comment a future author will
/// plausibly write, and the census counts call sites rather than the word "write".
#[test]
fn every_write_of_the_pinned_file_routes_through_splice() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sites: Vec<(String, String)> = Vec::new();

    for entry in std::fs::read_dir(&src_dir).expect("src/ is readable") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("module is readable");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        for line in text.lines() {
            let code = line.trim_start();
            // Strip `//`, `///` and `//!` alike: a doc comment is not a call site.
            if code.starts_with("//") {
                continue;
            }
            // `fs::copy` and `fs::rename` are here because an independent reviewer ran this
            // census with a WIDER pattern set than the one shipped above and got the same answer.
            // Same result, but the agreement was luck: they exist in no `src/` module today, so a
            // future `fs::copy(tmp, out_path)` would have written the pinned file and this census
            // would have reported "exactly 2" while missing it. A census is only as wide as its
            // producer list, and a found hole is a lower bound — the fix is the wider list, not
            // the reassuring count.
            if [
                "fs::write(",
                "File::create(",
                ".write_all(",
                "OpenOptions",
                "fs::copy(",
                "fs::rename(",
            ]
            .iter()
            .any(|p| code.contains(p))
            {
                sites.push((name.clone(), code.to_string()));
            }
        }
    }

    // Positive control: a census that can only return zero proves nothing. If the producer
    // patterns ever stop matching real code, this fires before the completeness assertion below
    // can pass vacuously.
    assert!(
        !sites.is_empty(),
        "the writer census found NO filesystem writes in src/, but probe-pin demonstrably writes \
         both a mutant and the pinned file. The search patterns no longer match real call sites, \
         so every assertion below this line would pass by finding nothing."
    );

    let files: Vec<&str> = sites.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(
        sites.len(),
        2,
        "the writer census changed: expected exactly 2 filesystem writes in src/ (mutate.rs's \
         scratch mutant, main.rs's pinned file), found {} in {files:?}. If a new write is \
         legitimate, decide which it is: a write of output.file MUST route through \
         block::splice, or the postcondition stops being unconditional and docs/probe-pin.md \
         plus the crate's own generated block are asserting something no longer true. \
         Sites: {sites:#?}",
        sites.len()
    );
    assert!(
        files.contains(&"mutate.rs") && files.contains(&"main.rs"),
        "the two writes are no longer the two expected producers: {files:?}"
    );

    let main_src = std::fs::read_to_string(src_dir.join("main.rs")).expect("main.rs is readable");
    let write_call = main_src
        .split_once("std::fs::write(")
        .expect("main.rs writes the pinned file")
        .1;
    let args = write_call
        .split_once(")\n")
        .map_or(write_call, |(before, _)| before);
    assert!(
        args.contains("block::splice("),
        "main.rs still writes the pinned file, but the bytes no longer come from block::splice. \
         The postcondition that refuses an unlocatable block is bypassed, and `run --write` can \
         again commit a block whose marker span its own `check` cannot find. Write args were: \
         {args}"
    );
}

/// The workspace root is CANONICAL at its one producer, because every consumer compares it
/// against a path that has been: `main`'s `manifest_rel` strips it from
/// `manifest_path.canonicalize()`, `resolve_contained` canonicalizes `base`, `scratch_dir`
/// canonicalizes both sides. A non-canonical root turns each of those realpath assertions back
/// into a lexical string comparison — the shape this crate has already repaired twice.
///
/// DISCLOSURE, measured: no input reaching `cargo locate-project --workspace` produces a
/// non-canonical root today. Cargo starts from `getcwd`, which the kernel answers physically,
/// so a workspace reached through a symlink still prints the resolved path (measured: run
/// through `/tmp/<link> -> the worktree`, exit 0, every row rendered). Arm 2 is therefore the
/// GUARD's shape measured on a synthetic pair, not a live escape — a found hole is a lower
/// bound, and this one is "not reached today", not "cannot be reached".
#[test]
#[cfg_attr(
    not(unix),
    ignore = "creates a symlink; off Unix that needs Developer Mode (Windows) or has no API at all"
)]
fn workspace_root_is_canonical() {
    let root = target::workspace_root().expect("workspace root");
    assert_eq!(
        root,
        root.canonicalize().expect("the root resolves"),
        "workspace_root must return a canonical path"
    );

    // arm 2 — the shape: a non-canonical base fails the prefix comparison that IS satisfied
    let base = tempfile::tempdir().unwrap();
    let real = base.path().join("real");
    std::fs::create_dir_all(real.join("crates")).unwrap();
    std::fs::write(real.join("crates/m.toml"), "version = 1\n").unwrap();
    let link = base.path().join("link");
    common::symlink(&real, &link).unwrap();

    let manifest_real = link.join("crates/m.toml").canonicalize().unwrap();
    assert!(
        manifest_real.strip_prefix(&link).is_err(),
        "the phenomenon: a canonicalized manifest path does not start with a symlinked root, so \
         the guard would report 'manifest is not inside the workspace root' for one that is"
    );
    assert!(
        manifest_real
            .strip_prefix(link.canonicalize().unwrap())
            .is_ok(),
        "and canonicalizing the root is what makes the comparison realpath-to-realpath"
    );
    // and the same rule already governs the other containment assertion, from the same base
    assert!(manifest::resolve_contained(&link, "crates/m.toml").is_ok());
}

/// The Tier-2 suite is `#[ignore]`d because GitHub's runners deny unprivileged user namespaces,
/// so GH CI never executes a real mount. That is a measured accommodation, and an accommodation
/// with nothing watching it decays: a Tier-2 test added without the attribute turns CI red for a
/// reason nobody will connect to namespaces, and — worse — the ignored set can widen into tests
/// that had no capability problem at all, which is "skipped in CI" quietly becoming "isolation
/// abandoned".
///
/// This guard lives in `pure_logic` on purpose: it is the suite CI actually runs, so the watcher
/// is not itself ignored. It pins the count, the attribute on EVERY test, and the exact reason
/// text — reword the reason and this fails, which is the intent: the reason is the only place a
/// reader is told the accommodation is about namespaces and how to run the suite anyway.
///
/// The companion control is the Tilt `probe-pin-e2e` resource, which runs the suite with
/// `--ignored` in the local venue, where the capability exists. Together: this test says "the
/// suite is still uniformly ignored and still says why", and Tilt says "and it still passes".
#[test]
fn tier2_suite_is_uniformly_ignored_with_the_measured_reason() {
    const EXPECTED: &str = "#[ignore = \"Tier 2: drives probe-pin through `unshare --map-root-user --mount`. CI runners deny unprivileged user namespaces (uid_map EPERM), so the whole suite runs locally only: `cargo test -p probe-pin --test isolation_e2e -- --ignored`.\"]";

    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/isolation_e2e.rs"),
    )
    .expect("the Tier-2 suite is readable");
    let lines: Vec<&str> = src.lines().collect();

    // Count the ATTRIBUTE, never the string: this file's own doc comments and a fixture label
    // both contain the text `#[ignore]`, and an unanchored match counts those too — the exact
    // trap that produced a 7-vs-1 discrepancy when this suite was first measured.
    let tests: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim() == "#[test]")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        tests.len(),
        17,
        "the Tier-2 suite changed size. Every test here needs the #[ignore] attribute (CI cannot \
         create a mount namespace); update this count deliberately, and keep the Tilt \
         `probe-pin-e2e` resource as the venue that actually runs them."
    );
    assert!(
        lines.iter().filter(|l| l.contains("#[ignore")).count() > tests.len(),
        "instrument check: this file is known to contain #[ignore] MENTIONS in prose as well as \
         attributes, so a raw substring count must exceed the attribute count. If it does not, \
         the mentions moved and this test's anchoring rationale needs re-reading."
    );

    for i in tests {
        let next = lines[i + 1].trim();
        assert_eq!(
            next,
            EXPECTED,
            "the #[test] at isolation_e2e.rs:{} is not followed by the expected #[ignore]. Every \
             Tier-2 test must carry it verbatim: CI denies unprivileged user namespaces, so an \
             unignored test here fails on every run for a reason unrelated to what it asserts.",
            i + 1
        );
    }
}

/// Driver-added. `docs/probe-pin.md`'s manifest example is the surface a first user COPIES, and
/// it had no mechanical coverage at all — an example is code that never runs, so nothing caught
/// that it (a) was not valid TOML (`text = "…" ; count = 1`; `;` is not a TOML separator) and
/// (b) named `package = "engine"`, which does not exist: `crates/engine` is package
/// `phase-engine` with `[lib] name = "engine"`, so `cargo test -p engine` fails to resolve.
/// Both shipped. This gate is why the fixtures were clean and the doc was not: fixtures get
/// parsed by other tests, the doc block got read by humans.
///
/// The example carries deliberate `…verbatim…` placeholders, so it cannot be RUN. What is
/// mechanically checkable is checked: it parses, it validates, and its package resolves.
#[test]
fn docs_manifest_example_parses_validates_and_names_a_real_package() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let doc = std::fs::read_to_string(root.join("docs/probe-pin.md")).expect("docs readable");
    let block = doc
        .split("```toml\n")
        .nth(1)
        .and_then(|rest| rest.split("\n```").next())
        .expect("docs carry a fenced toml manifest example");
    assert!(
        block.contains("[[probe]]") && block.contains("[target]"),
        "extracted the wrong fence, or the example lost its shape: {block:.200}"
    );

    let m: Manifest = toml::from_str(block)
        .unwrap_or_else(|e| panic!("the docs manifest example is not valid TOML: {e}"));
    manifest::validate(&m).expect("the docs manifest example must pass validate()");

    // Every workspace member is `crates/*` (root Cargo.toml: members = ["crates/*"]), so the
    // package set is readable without shelling to cargo — which would contend for the target
    // lock Tilt already holds.
    let mut names = Vec::new();
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/ readable") {
        let toml_path = entry.expect("dir entry").path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&toml_path) else {
            continue;
        };
        if let Ok(v) = toml::from_str::<toml::Value>(&text) {
            if let Some(n) = v
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
            {
                names.push(n.to_string());
            }
        }
    }
    assert!(
        names.contains(&"phase-engine".to_string()),
        "instrument check: the package scan must find a package it is known to contain, else a \
         missing package would pass vacuously. found: {names:?}"
    );
    assert!(
        !names.contains(&"engine".to_string()),
        "positive control for the exact defect: `engine` is the lib TARGET name, and if a package \
         by that name ever exists this test stops discriminating"
    );
    assert!(
        names.contains(&m.target.package),
        "the docs example names package '{}', which is not a workspace package. `cargo test -p \
         {}` would abort for anyone copying it. found: {names:?}",
        m.target.package,
        m.target.package
    );
}

/// Driver-added beyond the final review-impl finding set, after MEASURING the escape the
/// reviewer had adjudicated as hardening: `[output].file = "../probepin-run/…"` + `run --write`
/// exited 0 and spliced the block into a file outside the workspace root. That is not a
/// containment nicety — `check` resolves `[output].file` against the root, so a pin written
/// outside it is one no checkout and no CI job can re-measure.
#[test]
fn output_file_must_be_workspace_relative() {
    let text = std::fs::read_to_string(fixtures().join("dogfood.toml")).unwrap();
    for escape in [
        "../probepin-run/escape.md",
        "/tmp/escape.md",
        "crates/../../escape.md",
    ] {
        let edited = text.replace(
            "file   = \"crates/probe-pin/tests/fixtures/dogfood_block.md\"",
            &format!("file   = \"{escape}\""),
        );
        assert_ne!(
            edited, text,
            "the fixture edit must apply, or this is vacuous"
        );
        let m = toml::from_str::<Manifest>(&edited).expect("manifest parses");
        assert_eq!(
            m.output.file, escape,
            "the escaping path reached the struct"
        );
        let e = manifest::validate(&m).unwrap_err();
        assert!(
            e.to_string().contains("is not workspace-relative"),
            "{escape} was accepted: {e}"
        );
    }
    // positive: the shipping path is workspace-relative and still validates, so the refusal is
    // about the escape and not about `[output].file` being checked at all
    assert!(manifest::validate(&toml::from_str::<Manifest>(&text).unwrap()).is_ok());
}

/// Final review-impl, MINOR-5: `timeout_secs = 0` disables the guard instead of tightening it.
/// The shell behaviour is MEASURED here, not argued — it is the whole reason for the refusal.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "spawns GNU timeout(1); see docs/probe-pin.md"
)]
fn timeout_zero_is_refused() {
    let rc = |secs: &str, sleep: &str| {
        Command::new("timeout")
            .args(["-k", "5", secs, "sleep", sleep])
            .status()
            .unwrap()
            .code()
            .unwrap()
    };
    assert_eq!(
        rc("0", "0.2"),
        0,
        "`timeout 0` runs the child to completion"
    );
    assert_eq!(rc("1", "5"), 124, "a positive timeout fires");

    let text = std::fs::read_to_string(fixtures().join("dogfood.toml")).unwrap();
    let zero = text.replace("timeout_secs = 60", "timeout_secs = 0");
    assert_ne!(
        zero, text,
        "the fixture edit must apply, or this arm is vacuous"
    );
    let m = toml::from_str::<Manifest>(&zero).expect("manifest parses");
    assert_eq!(m.target.timeout_secs, 0);
    let e = manifest::validate(&m).unwrap_err();
    assert!(e.to_string().contains("disables `timeout`"), "{e}");
    // positive: the shipping value validates
    assert!(manifest::validate(&toml::from_str::<Manifest>(&text).unwrap()).is_ok());
}

/// A blank `[target].filter` is a manifest defect that neither `filter_match` mode reports AS one.
///
/// Measured before the refusal existed, running a real manifest with `filter = ""` and
/// `filter_match = "substring"`: the run widened to the whole target binary and probe-pin exited 2
/// — but with "the control probe 'P0_control' FAILED with exit 101 … the instrument is broken",
/// because a test in the widened set happened to fail. Nothing looked at the filter. Had the
/// widened suite passed, every row would have rendered green while measuring a suite the block
/// names as one test. The `exact` mode fails the other way: zero matches, which the execution
/// floor reports as a target that ran nothing.
#[test]
fn blank_filter_is_refused() {
    let text = std::fs::read_to_string(fixtures().join("dogfood.toml")).unwrap();
    let blank = text.replace("filter       = \"ppfixture_census\"", "filter       = \"\"");
    assert_ne!(
        blank, text,
        "the fixture edit must apply, or this arm is vacuous"
    );
    let m = toml::from_str::<Manifest>(&blank).expect("manifest parses");
    assert_eq!(m.target.filter, "");
    let e = manifest::validate(&m).unwrap_err();
    assert!(e.to_string().contains("[target].filter is blank"), "{e}");

    // Whitespace is blank too — a filter of spaces names no test either, and `trim` is what makes
    // the refusal about NAMING rather than about the empty string.
    let spaces = text.replace(
        "filter       = \"ppfixture_census\"",
        "filter       = \"   \"",
    );
    let m = toml::from_str::<Manifest>(&spaces).expect("manifest parses");
    assert!(
        manifest::validate(&m)
            .unwrap_err()
            .to_string()
            .contains("[target].filter is blank"),
        "a whitespace-only filter must be refused for the same reason"
    );

    // Positive control: the shipping filter still validates, so the refusal is discriminating and
    // not a blanket rejection of the field.
    assert!(manifest::validate(&toml::from_str::<Manifest>(&text).unwrap()).is_ok());
}

/// The control row is keyed on probe IDENTITY, so a MUTATION probe can never claim to be one.
///
/// Discriminating by construction: both probes below carry the identical verdict
/// `Pass { mounts_reached: 0 }`. Only their identity differs. Before this change `firing_cell`
/// read the count, so both rendered `(control; no mounts)` — the mutation probe's row asserting
/// it had no mutations, in a committed evidence artifact, with a digest that agrees because the
/// digest pins the same count.
///
/// The zero-mount mutation probe is not reachable through `main` today (`validate` refuses an
/// empty mutation file list, so `mounts.len() >= 1`), which is exactly why the test constructs the
/// state directly: the property being pinned is that the ROW cannot lie, not that the current call
/// graph happens not to produce a liar.
#[test]
fn a_mutation_probe_can_never_render_as_the_control() {
    let text = std::fs::read_to_string(fixtures().join("dogfood.toml")).unwrap();
    let m = toml::from_str::<Manifest>(&text).expect("manifest parses");
    let control = &m.probes[0];
    let mutating = &m.probes[1];
    assert!(
        control.mutations.is_empty() && !mutating.mutations.is_empty(),
        "fixture shape assumed by this test: probe 0 is the control, probe 1 mutates"
    );

    let render_with = |probe: &Probe| {
        let one = Manifest {
            probes: vec![probe.clone()],
            ..m.clone()
        };
        let outcomes = vec![Outcome {
            id: probe.id.clone(),
            rc: 0,
            // The SAME verdict for both probes: identity is the only variable.
            verdict: Verdict::Pass { mounts_reached: 0 },
        }];
        block::render(&one, "m.toml", "d", &[], &outcomes, &[])
    };

    assert!(
        render_with(control).contains("(control; no mounts)"),
        "the control's row must still say so"
    );
    let mutant_row = render_with(mutating);
    assert!(
        !mutant_row.contains("(control; no mounts)"),
        "a probe WITH mutations rendered as the control on a zero count. The row is deciding \
         probe identity from a measured number, so an evidence artifact states that a mutation \
         probe made no mutations. Row was:\n{mutant_row}"
    );
    assert!(
        mutant_row.contains("mount reached: 0 file(s)"),
        "and it must report the count it actually measured, however unexpected. Row was:\n{mutant_row}"
    );
}

/// The table can never contain fewer rows than the manifest declares probes.
///
/// `digest` records an outcome-less probe as `rc = -1`, `verdict = "unmeasured"`. `render` used to
/// `continue` past it. That disagreement is the silent kind: a manifest declares N probes, the
/// block shows N-1 rows, and `check` reports CLEAN — the committed and generated blocks omit the
/// same row and stamp the same digest, so the drift detector agrees with the loss.
///
/// Not reachable through `main` (the mismatch gate at `main.rs:190` returns first), which is why
/// this asserts on `render` directly. `render` is public and this input is legitimate: the
/// projection-surface test calls it with `&[]` outcomes.
#[test]
fn a_probe_with_no_outcome_gets_a_visible_unmeasured_row() {
    let text = std::fs::read_to_string(fixtures().join("dogfood.toml")).unwrap();
    let m = toml::from_str::<Manifest>(&text).expect("manifest parses");
    let declared = m.probes.len();
    assert!(declared >= 2, "fixture must declare several probes");

    // Every probe measured EXCEPT the last one.
    let outcomes: Vec<Outcome> = m.probes[..declared - 1]
        .iter()
        .map(|p| Outcome {
            id: p.id.clone(),
            rc: 0,
            verdict: Verdict::Pass { mounts_reached: 1 },
        })
        .collect();
    let rendered = block::render(&m, "m.toml", "d", &[], &outcomes, &[]);

    let rows = rendered
        .lines()
        .filter(|l| l.starts_with("// | ") && !l.contains("| probe |"))
        .count();
    assert_eq!(
        rows, declared,
        "the table dropped a declared probe instead of rendering it. A block that silently loses \
         evidence still passes `check`, because the committed copy lost the same row.\n{rendered}"
    );

    let dropped = &m.probes[declared - 1].id;
    let row = rendered
        .lines()
        .find(|l| l.contains(dropped.as_str()))
        .unwrap_or_else(|| panic!("no row for the unmeasured probe:\n{rendered}"));
    assert!(
        row.contains("unmeasured"),
        "the unmeasured probe's row must SAY it is unmeasured, matching what digest records for \
         the same input: {row}"
    );

    // Discriminating: a fully-measured run must not gain a phantom row, or the count assertion
    // above would pass for a render that emits an extra line per probe.
    let all: Vec<Outcome> = m
        .probes
        .iter()
        .map(|p| Outcome {
            id: p.id.clone(),
            rc: 0,
            verdict: Verdict::Pass { mounts_reached: 1 },
        })
        .collect();
    let full = block::render(&m, "m.toml", "d", &[], &all, &[]);
    assert_eq!(
        full.lines()
            .filter(|l| l.starts_with("// | ") && !l.contains("| probe |"))
            .count(),
        declared,
        "a fully-measured render must have exactly one row per probe:\n{full}"
    );
    assert!(
        !full.contains("unmeasured"),
        "and none of them may be marked unmeasured:\n{full}"
    );
}

// ------------------------------------------------------------------ row 19

/// Row 19: `assert_count` runs on the MUTANT, after all mutations have composed.
#[test]
fn assert_count() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    write(dir.path(), "f.txt", "alpha\nbeta\nalpha\n");
    let mk = |text: &str, count: usize| {
        probe(&format!(
            "id='P'\n[[mutation]]\nkind='replace'\nfile='f.txt'\nfind='beta'\nreplace='alpha'\n\
             [[assert_count]]\nfile='f.txt'\ntext={text:?}\ncount={count}\n[expect]\noutcome='pass'\n"
        ))
    };
    assert!(mutate::apply(&mk("alpha", 3), dir.path(), scratch.path()).is_ok());
    // off-by-one
    let e = abort_of(mutate::apply(&mk("alpha", 2), dir.path(), scratch.path()).unwrap_err());
    assert!(
        matches!(
            e,
            Abort::AssertCount {
                expected: 2,
                found: 3,
                ..
            }
        ),
        "{e}"
    );
    // present in the ORIGINAL, absent from the mutant
    let e = abort_of(mutate::apply(&mk("beta", 1), dir.path(), scratch.path()).unwrap_err());
    assert!(
        matches!(
            e,
            Abort::AssertCount {
                expected: 1,
                found: 0,
                ..
            }
        ),
        "{e}"
    );
}

// ------------------------------------------------------------------ row 20

/// Row 20: probe-pin must never create files under the tree it is measuring. The assert is
/// on containment, not inequality — a parent-blind `path != workspace_root` accepts every
/// subdirectory of the worktree.
#[test]
fn scratch_outside() {
    let ws = tempfile::tempdir().unwrap();
    assert!(isolate::scratch_dir(&std::env::temp_dir(), ws.path()).is_ok());

    let inside = ws.path().join("x");
    std::fs::create_dir_all(&inside).unwrap();
    let e = isolate::scratch_dir(&inside, ws.path()).unwrap_err();
    assert!(matches!(e, Abort::ScratchInsideWorkspace { .. }), "{e}");
    // TRIVIALIZE control: the scratch is never EQUAL to the root, so equality catches nothing
    match e {
        Abort::ScratchInsideWorkspace { scratch, workspace } => assert_ne!(scratch, workspace),
        _ => unreachable!(),
    }
}

// ------------------------------------------- row 20's companion: the escaping key (MAJOR-1)

/// Row 20 constrains the CONTAINER (`scratch` is outside the workspace). This constrains the
/// KEYS joined onto it: `mutate::apply` writes `scratch.join(&probe.id).join(rel)`, so a `..`
/// in EITHER half walks back out of the container the row-20 assert just certified, into the
/// tree being measured. `verify_fingerprint` cannot see the id half either — the escaped write
/// is not in `probe.touched()`.
///
/// Arm 1 and arm 3 EXHIBIT the escape through the shipping `mutate::apply`. Arms 2 and 4 drive
/// the guarded path in pipeline order (§7 step 1 `validate`, then step 8 `apply`) and assert
/// both halves of the fix: the abort fires AND nothing appears outside the scratch dir.
#[test]
fn path_keys_cannot_escape_the_scratch_dir() {
    let base = tempfile::tempdir().unwrap();
    let ws = base.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    write(&ws, "f.txt", "alpha\n");
    write(&ws, "VICTIM2.rs", "alpha\n");
    let scratch = tempfile::tempdir().unwrap();

    // `../` × depth(dir), then down into the workspace: from `dir`, out to the filesystem root
    // and back in. Every component exists, so the kernel resolves the whole path.
    let escape_to = |dir: &Path, name: &str| {
        let up = dir
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .count();
        format!(
            "{}{}/{name}",
            "../".repeat(up),
            ws.strip_prefix("/").unwrap().display()
        )
    };
    let listing = || {
        let mut v: Vec<String> = std::fs::read_dir(&ws)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    let manifest_with = |block: &str| -> Manifest {
        toml::from_str(&format!(
            "version = 1\n\
             [target]\nmode='runtime-read'\npackage='p'\ntest='t'\nfilter='f'\nfilter_match='substring'\n\
             [output]\nfile='o.md'\nmarker='PROBE-PIN'\n\
             [[probe]]\nid='P0_control'\n[probe.expect]\noutcome='pass'\n{block}"
        ))
        .expect("hostile manifest parses — the schema does not constrain these fields")
    };
    // §7 in miniature: validate, then apply every non-control probe.
    let guarded = |m: &Manifest| -> anyhow::Result<()> {
        manifest::validate(m)?;
        for p in m.probes.iter().filter(|p| !p.mutations.is_empty()) {
            mutate::apply(p, &ws, scratch.path())?;
        }
        Ok(())
    };
    let hostile_id = escape_to(scratch.path(), "VICTIM.rs");
    let id_manifest = manifest_with(&format!(
        "[[probe]]\nid={hostile_id:?}\n\
         [[probe.mutation]]\nkind='replace'\nfile='f.txt'\nfind='alpha'\nreplace='beta'\n\
         [probe.expect]\noutcome='pass'\n"
    ));

    // arm 1 — the phenomenon: the JOIN escapes, so the write lands INSIDE the measured
    // workspace. `mutate::apply` computed exactly this destination and wrote it; the write-side
    // containment assert now refuses it first (arm 1b), so the escape is performed here to stay
    // MEASURED rather than merely asserted about.
    let escaped_dest = scratch.path().join(&hostile_id).join("f.txt");
    std::fs::create_dir_all(escaped_dest.parent().unwrap()).unwrap();
    std::fs::write(&escaped_dest, "beta\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(ws.join("VICTIM.rs/f.txt")).unwrap(),
        "beta\n",
        "the escape must be real, or arms 1b and 2 pin nothing"
    );
    std::fs::remove_dir_all(ws.join("VICTIM.rs")).unwrap();

    // arm 1b — the write-side guard: `mutate::apply` refuses that destination by containment,
    // whatever validation did or did not do about the id's charset
    let e = mutate::apply(&id_manifest.probes[1], &ws, scratch.path()).unwrap_err();
    assert!(
        format!("{e}").contains("outside probe-pin's scratch directory"),
        "{e}"
    );
    assert_eq!(listing(), ["VICTIM2.rs", "f.txt"], "nothing may be written");

    // arm 2 — the guard: refused, and NO file appears outside the scratch dir
    let e = guarded(&id_manifest).unwrap_err();
    assert!(
        format!("{e}").contains("is not a plain name"),
        "the id must be refused by content: {e}"
    );
    assert_eq!(listing(), ["VICTIM2.rs", "f.txt"], "nothing may be written");

    // arm 3 — the phenomenon, `.join(rel)` half: the mutant lands ON a workspace file
    let hostile_file = escape_to(&scratch.path().join("P1_escaping_file"), "VICTIM2.rs");
    let file_manifest = manifest_with(&format!(
        "[[probe]]\nid='P1_escaping_file'\n\
         [[probe.mutation]]\nkind='replace'\nfile={hostile_file:?}\nfind='alpha'\nreplace='beta'\n\
         [probe.expect]\noutcome='pass'\n"
    ));
    let escaped_dest = scratch.path().join("P1_escaping_file").join(&hostile_file);
    std::fs::create_dir_all(escaped_dest.parent().unwrap()).unwrap();
    std::fs::write(&escaped_dest, "beta\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(ws.join("VICTIM2.rs")).unwrap(),
        "beta\n",
        "the mutant must have been written over the workspace file"
    );
    write(&ws, "VICTIM2.rs", "alpha\n");

    // arm 3b — the same write-side guard, on the `.join(rel)` half
    let e = mutate::apply(&file_manifest.probes[1], &ws, scratch.path()).unwrap_err();
    assert!(
        format!("{e}").contains("outside probe-pin's scratch directory"),
        "{e}"
    );

    // arm 4 — the guard, same half
    let e = guarded(&file_manifest).unwrap_err();
    assert!(
        format!("{e}").contains("is not a workspace-relative path"),
        "the mutation file must be refused by content: {e}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("VICTIM2.rs")).unwrap(),
        "alpha\n",
        "nothing may be written"
    );
    assert_eq!(listing(), ["VICTIM2.rs", "f.txt"]);

    // positive: an ordinary id and an ordinary relative file still validate and materialize
    let ok = manifest_with(
        "[[probe]]\nid='P1_ordinary'\n\
         [[probe.mutation]]\nkind='replace'\nfile='f.txt'\nfind='alpha'\nreplace='beta'\n\
         [probe.expect]\noutcome='pass'\n",
    );
    guarded(&ok).expect("the shipping shape must survive the guard");
    assert!(scratch.path().join("P1_ordinary/f.txt").exists());
    assert_eq!(listing(), ["VICTIM2.rs", "f.txt"]);
}

/// Maintainer review on #7315, blocking: "add a regression test proving marker/newline injection
/// is rejected and that `run --write` cannot produce an uncheckable block". Those are two
/// different claims and they are proven at two different layers, because closing only the first
/// leaves the second conditional on probe-pin having seen every producer in advance.
///
/// Layer 1 — the manifest fields, refused before any target runs. Layer 2 — the bytes actually
/// written, refused by `splice` itself. Layer 2 is what makes the guarantee unconditional: an
/// instrument's `--version` output and the `provenance` paths lifted from the target's panic text
/// also reach rendered lines, and neither can be validated in advance.
#[test]
fn write_can_never_produce_a_block_its_own_check_cannot_locate() {
    let file = Path::new("out.md");
    let text = "above\n// PROBE-PIN:BEGIN old\n// stale\n// PROBE-PIN:END\nbelow\n";

    // Layer 1: manifest-derived injection is refused, including the maintainer's exact string.
    let manifest = std::fs::read_to_string(fixtures().join("dogfood.toml")).unwrap();
    for hostile in ["\nPROBE-PIN:END", "PROBE-PIN:END", "boom\nsecond"] {
        let edited = manifest.replace(
            r#"anchor  = ["CENSUS VIOLATED: expected 2 sites, got 202"]"#,
            &format!("anchor  = [{}]", serde_json::to_string(hostile).unwrap()),
        );
        assert_ne!(
            edited, manifest,
            "the fixture edit must apply, or this is vacuous"
        );
        let m: Manifest = toml::from_str(&edited).expect("manifest parses");
        // `validate_block_text` is the guard for values that reach the BLOCK, and it is separate
        // from `validate` because it needs the manifest's workspace-relative path — which is
        // itself one of the rendered values. Both run before any target, at `main.rs:113-114`.
        let msg =
            match manifest::validate_block_text(&m, "crates/probe-pin/tests/fixtures/dogfood.toml")
            {
                Ok(()) => panic!("{hostile:?} was ACCEPTED and would be rendered into the block"),
                Err(e) => e.to_string(),
            };
        assert!(
            msg.contains("control character") || msg.contains("marker tag"),
            "{hostile:?} was accepted: {msg}"
        );
    }

    // Layer 2: a generated block carrying a second END is refused AT THE WRITE, no manifest
    // involved — this is the arm that covers producers probe-pin cannot inspect beforehand.
    let injected = "// PROBE-PIN:BEGIN new\n// | p | m | pass | pass | // PROBE-PIN:END | — |\n// PROBE-PIN:END";
    let err = block::splice(text, "PROBE-PIN", file, injected)
        .expect_err("a block carrying a second END must never be returned for writing");
    assert!(
        matches!(abort_of(err.into()), Abort::MarkerNotUnique { begins, ends, .. } if begins == 1 && ends == 2),
        "the refusal must name the marker imbalance it found"
    );

    // Positive control: the same splice with a well-formed block still succeeds, so the guard
    // above is refusing the injection and not simply refusing everything.
    let clean = "// PROBE-PIN:BEGIN new\n// | p | m | pass | pass | fired | — |\n// PROBE-PIN:END";
    let out = block::splice(text, "PROBE-PIN", file, clean).expect("a well-formed block splices");
    assert!(out.starts_with("above\n") && out.ends_with("below\n"));
    // and the result is locatable, which is the property the whole test is about
    block::extract(&out, "PROBE-PIN", file).expect("the written file must be locatable");
}

// ------------------------------------------------------------------ row 22

/// Row 22: the splice preserves every byte outside the marker pair — prose above, prose
/// below, and the file's own line endings.
#[test]
fn splice() {
    let file = Path::new("out.md");
    let text = "above\r\n// PROBE-PIN:BEGIN old\r\n// stale\r\n// PROBE-PIN:END\r\nbelow\r\n";
    let out = block::splice(
        text,
        "PROBE-PIN",
        file,
        "// PROBE-PIN:BEGIN new\n// PROBE-PIN:END",
    )
    .unwrap();
    assert_eq!(
        out,
        "above\r\n// PROBE-PIN:BEGIN new\n// PROBE-PIN:END\r\nbelow\r\n"
    );
    assert!(out.starts_with("above\r\n") && out.ends_with("below\r\n"));

    for (bad, begins, ends) in [
        ("// PROBE-PIN:BEGIN x\n", 1, 0),
        (
            "// PROBE-PIN:BEGIN x\n// PROBE-PIN:BEGIN y\n// PROBE-PIN:END\n",
            2,
            1,
        ),
        ("// PROBE-PIN:END\n// PROBE-PIN:BEGIN x\n", 1, 1), // END before BEGIN
    ] {
        let e = block::locate(bad, "PROBE-PIN", file).unwrap_err();
        assert!(
            matches!(e, Abort::MarkerNotUnique { begins: b, ends: n, .. } if b == begins && n == ends),
            "{bad:?} -> {e}"
        );
    }
}

// ------------------------------------------------------------------ row 23

/// Row 23: the render is byte-deterministic. The guard PARSES TWICE and renders each: a
/// render-twice-from-one-parse guard scores identically with and without the defect, because
/// a `HashMap`'s iteration order is stable within one instance.
#[test]
fn idempotent() {
    let ids: Vec<String> = load("unsorted_ids.toml")
        .probes
        .iter()
        .map(|p| p.id.clone())
        .collect();
    // MINOR-F: the fixture precondition is MEASURED, not merely stated. The corpus ids
    // P0..P9 are already sorted, so a sorted fixture makes the TRIVIALIZE arm a no-op.
    assert_ne!(ids, {
        let mut s = ids.clone();
        s.sort();
        s
    });

    let render_once = || {
        let m = load("unsorted_ids.toml");
        let outcomes: Vec<Outcome> = m
            .probes
            .iter()
            .map(|p| Outcome {
                id: p.id.clone(),
                rc: 0,
                verdict: Verdict::Pass { mounts_reached: 1 },
            })
            .collect();
        // The DIGEST, not just the table: `[target].env` reaches the block only through the
        // digest, so a `HashMap` there is invisible to a render-only guard (MINOR-4). It is
        // also invisible within ONE parse — `RandomState` is per-instance — hence the loop
        // below, and the fixture's four env keys.
        let digest = block::digest(&m, "m.toml", &outcomes, &[], &[]).unwrap();
        (
            block::render(&m, "m.toml", &digest, &[], &outcomes, &[]),
            digest,
        )
    };
    let (a, b) = (render_once(), render_once());
    assert_eq!(a, b);
    for _ in 0..16 {
        assert_eq!(render_once(), a, "two parses must render identical bytes");
    }
    // the rows appear in DECLARATION order, which is what `check` compares against
    let rows: Vec<&str> = a.0.lines().filter(|l| l.starts_with("// | P")).collect();
    assert!(
        rows[0].contains("P0_control")
            && rows[1].contains("P9_zulu")
            && rows[2].contains("P1_alpha")
    );
}

// ------------------------------------------------------------------ rows 24, 25, 27

fn digest_of(
    m: &Manifest,
    counts: &[ProjectionCount],
    instruments: &[(Instrument, String)],
) -> String {
    let outcomes: Vec<Outcome> = m
        .probes
        .iter()
        .map(|p| Outcome {
            id: p.id.clone(),
            rc: 0,
            verdict: Verdict::Pass { mounts_reached: 1 },
        })
        .collect();
    block::digest(m, "m.toml", &outcomes, counts, instruments).unwrap()
}

/// Row 24: the digest pins what was MEASURED, not just the outcome. Narrowing `[target]`'s
/// filter with identical verdicts flips it, and subsetting a projection's `paths` with an
/// identical count flips it — the two cases a digest over outcomes alone cannot see.
#[test]
fn digest_target() {
    let base = load("projection.toml");
    let counts = [ProjectionCount {
        id: "instrument_sites".into(),
        count: 3,
    }];
    let instruments = [(Instrument::Toolchain, "rustc 1.97.0".to_string())];
    let d0 = digest_of(&base, &counts, &instruments);

    let mut narrowed = base.clone();
    narrowed.target.filter = "ppfixture_census_narrower".into();
    assert_ne!(d0, digest_of(&narrowed, &counts, &instruments));

    let mut subset = base.clone();
    subset.projections[0].paths = vec!["crates/probe-pin/tests".into()];
    assert_ne!(d0, digest_of(&subset, &counts, &instruments));

    // positive: an unrelated COMMENT in the manifest is not an input
    let text = std::fs::read_to_string(fixtures().join("projection.toml")).unwrap();
    let commented: Manifest = toml::from_str(&format!("# an unrelated comment\n{text}")).unwrap();
    assert_eq!(d0, digest_of(&commented, &counts, &instruments));
    // determinism
    assert_eq!(d0, digest_of(&base, &counts, &instruments));
}

/// Row 25: injection-safe by construction. JSON escapes NUL as `\u0000` and keeps every field
/// in its own slot; a hand-rolled NUL-joined serializer gives the two manifests below —
/// which say DIFFERENT things — one and the same byte string, `P1\0claim\0c`.
#[test]
fn digest_inject() {
    let mk = |id: &str, claim: &str| {
        let mut m = load("dogfood.toml");
        m.probes[1].id = id.to_string();
        m.probes[1].claim = claim.to_string();
        digest_of(&m, &[], &[])
    };
    // one colliding pair per candidate joiner — every single-delimiter joiner has one
    assert_ne!(mk("P1\u{0}claim", "c"), mk("P1", "claim\u{0}c"));
    assert_ne!(mk("P1+claim", "c"), mk("P1", "claim+c"));
    // and a field-boundary shift is still visible
    assert_ne!(mk("P1\u{0}claim\u{0}c", "c"), mk("P1\u{0}claim", "c"));
    assert_ne!(mk("P1\u{0}claim", "c"), mk("P1", "c"));
}

/// Row 27: BOTH instruments are digest inputs. With every verdict and count identical, each
/// version string alone flips the digest.
#[test]
fn instrument_digest() {
    let m = load("projection.toml");
    let counts = [ProjectionCount {
        id: "instrument_sites".into(),
        count: 3,
    }];
    let v = |rustc: &str, ag: &str| {
        digest_of(
            &m,
            &counts,
            &[
                (Instrument::Toolchain, rustc.to_string()),
                (Instrument::AstGrep, ag.to_string()),
            ],
        )
    };
    let base = v("rustc 1.97.0", "ast-grep 0.44.1");
    assert_ne!(base, v("rustc 1.98.0", "ast-grep 0.44.1"));
    assert_ne!(base, v("rustc 1.97.0", "ast-grep 0.45.0"));
}

// ------------------------------------------------------------------ rows 28, 29

/// The five measured skew arms, shared by rows 28 and 29.
fn skew_arms() -> Vec<(&'static str, String, String)> {
    let blk = |rustc: &str, ag: &str, count: usize, cell: &str| {
        format!(
            "// PROBE-PIN:BEGIN manifest=m.toml digest=sha256:0123456789abcdef\n\
             // instrument rustc = {rustc}\n\
             // instrument ast-grep = {ag}\n\
             // | probe | mutation | expect | verdict | firing assertion (anchor) | provenance |\n\
             // |---|---|---|---|---|---|\n\
             // | P1 | f.rs ×1 | fail | fail | {cell} | census.rs |\n\
             // `TargetPin::Player` is constructed or matched at {count} sites.\n\
             // PROBE-PIN:END"
        )
    };
    let base = blk("rustc 1.97.0", "ast-grep 0.44.1", 3, "ANCHOR");
    vec![
        ("clean", base.clone(), base.clone()),
        (
            "code drift only",
            base.clone(),
            blk("rustc 1.97.0", "ast-grep 0.44.1", 3, "OTHER"),
        ),
        (
            "toolchain moved, numbers same",
            base.clone(),
            blk("rustc 1.98.0", "ast-grep 0.44.1", 3, "ANCHOR"),
        ),
        (
            "ast-grep moved AND 3->6",
            base.clone(),
            blk("rustc 1.97.0", "ast-grep 0.45.0", 6, "ANCHOR"),
        ),
        (
            "both moved, numbers same",
            base,
            blk("rustc 1.98.0", "ast-grep 0.45.0", 3, "ANCHOR"),
        ),
    ]
}

/// Row 28: an instrument change is not code drift. Both instruments participate, and when
/// both moved the message lists BOTH in one `Instrument(Vec<…>)` — no precedence rule.
#[test]
fn instrument_skew() {
    let arms = skew_arms();
    let got: Vec<CheckOutcome> = arms.iter().map(|(_, a, b)| block::check(a, b)).collect();

    assert_eq!(got[0], CheckOutcome::Clean, "Clean must be reachable");
    match &got[1] {
        CheckOutcome::Drift {
            cause,
            numbers_moved,
            ..
        } => {
            assert_eq!(*cause, DriftCause::Code);
            assert!(numbers_moved);
        }
        o => panic!("code drift -> {o:?}"),
    }
    let instrument_of = |o: &CheckOutcome| match o {
        CheckOutcome::Drift {
            cause: DriftCause::Instrument(c),
            numbers_moved,
            ..
        } => (
            c.iter().map(|c| c.instrument).collect::<Vec<_>>(),
            *numbers_moved,
        ),
        o => panic!("expected an instrument drift, got {o:?}"),
    };
    assert_eq!(instrument_of(&got[2]), (vec![Instrument::Toolchain], false));
    assert_eq!(instrument_of(&got[3]), (vec![Instrument::AstGrep], true));
    assert_eq!(
        instrument_of(&got[4]),
        (vec![Instrument::Toolchain, Instrument::AstGrep], false)
    );
    assert!(block::report(&got[4], "m.toml").contains("rustc 1.97.0 -> rustc 1.98.0"));
    assert!(block::report(&got[4], "m.toml").contains("ast-grep 0.44.1 -> ast-grep 0.45.0"));
}

/// Row 29: `run --write` refuses IFF an instrument moved AND a measured number moved — the
/// laundering case, where re-stamping would record 6 sites under a new tool and leave `check`
/// green. Refusing on ANY instrument change instead would make a toolchain bump unwritable.
#[test]
fn write_refusal() {
    let arms = skew_arms();
    let refused: Vec<bool> = arms
        .iter()
        .map(|(_, a, b)| block::write_refusal(&block::check(a, b), a, b).is_some())
        .collect();
    assert_eq!(refused, vec![false, false, false, true, false]);

    let (_, a, b) = &arms[3];
    let abort = block::write_refusal(&block::check(a, b), a, b).unwrap();
    let text = abort.to_string();
    assert!(
        text.contains("ast-grep 0.44.1 -> ast-grep 0.45.0"),
        "{text}"
    );
    assert!(
        text.contains("at 3 sites") && text.contains("at 6 sites"),
        "{text}"
    );
    assert!(
        text.contains("delete the PROBE-PIN:BEGIN…END block"),
        "{text}"
    );
}

/// Row 29's escape (final review-impl, MAJOR-3): a projection sentence is free-form manifest
/// text rendered into the same line-space as the `instrument` lines, so classifying by TEXT
/// PREFIX let a sentence beginning `instrument ` be read as an instrument line — excluded from
/// the "a measured number moved" comparison, which silently disabled the write refusal and made
/// `check` print "Measured numbers are unchanged" about a number that had moved 3 -> 6.
///
/// The two arms differ by ONE CHARACTER (`instrument` vs `Instrument`), and both blocks come
/// out of the real `render`, so classification is pinned where it is produced.
#[test]
fn projection_sentence_may_start_with_the_word_instrument() {
    let outcomes = vec![Outcome {
        id: "P0_control".to_string(),
        rc: 0,
        verdict: Verdict::Pass { mounts_reached: 0 },
    }];
    let render = |head: &str, ag: &str, count: usize| {
        let mut m = load("projection.toml");
        m.projections[0].sentence = format!("{head} sites counted: {{count}}");
        let counts = vec![ProjectionCount {
            id: m.projections[0].id.clone(),
            count,
        }];
        block::render(
            &m,
            "m.toml",
            "0123456789abcdef",
            &[
                (Instrument::Toolchain, "rustc 1.97.0".to_string()),
                (Instrument::AstGrep, ag.to_string()),
            ],
            &outcomes,
            &counts,
        )
    };

    for head in ["instrument", "Instrument"] {
        let committed = render(head, "ast-grep 0.44.1", 3);
        let generated = render(head, "ast-grep 0.45.0", 6);
        // reach guard: the sentence really is rendered, and the number really did move
        assert!(
            committed.contains(&format!("// {head} sites counted: 3")),
            "{committed}"
        );
        assert!(
            generated.contains(&format!("// {head} sites counted: 6")),
            "{generated}"
        );

        let outcome = block::check(&committed, &generated);
        assert!(
            block::write_refusal(&outcome, &committed, &generated).is_some(),
            "a sentence starting {head:?} must not disable the write refusal"
        );
        assert!(
            block::report(&outcome, "m.toml").contains("cannot be attributed"),
            "a sentence starting {head:?} must not make `check` claim the numbers held still"
        );
    }

    // the layout the classification rests on: BEGIN, then the instrument lines CONTIGUOUSLY,
    // and the projection sentence outside that run however it is spelled.
    let block = render("instrument", "ast-grep 0.44.1", 3);
    let lines: Vec<&str> = block.lines().collect();
    assert!(lines[0].contains(":BEGIN manifest="), "{:?}", lines[0]);
    assert!(lines[1].starts_with("// instrument rustc = "));
    assert!(lines[2].starts_with("// instrument ast-grep = "));
    assert!(!lines[3].starts_with("// instrument "), "{:?}", lines[3]);
}

// ------------------------------------------------------------------ rows 26, 30

/// Row 26: fail-closed on a missing projection tool — probe-pin will not emit a block whose
/// projection sentence is silently absent. Both `run --write` and `check` reach this through
/// the same step-4 call, because `used_instruments` is what puts ast-grep on the list.
#[test]
fn proj_missing() {
    let m = load("projection.toml");
    assert_eq!(
        block::used_instruments(&m),
        vec![Instrument::Toolchain, Instrument::AstGrep]
    );
    let missing = fixtures().join("astgrep_does_not_exist");
    let e =
        project::instrument_version(Instrument::AstGrep, missing.to_str().unwrap()).unwrap_err();
    assert!(matches!(e, Abort::ProjectionToolMissing { .. }), "{e}");
    // present but broken is ALSO fail-closed, not a warn-and-continue
    let broken = fixtures().join("astgrep_broken.sh");
    let e = project::count(&m.projections[0], &fixtures(), broken.to_str().unwrap()).unwrap_err();
    assert!(matches!(e, Abort::ProjectionToolMissing { .. }), "{e}");

    // positive: a working tool renders the sentence with the measured count
    let ok = fixtures().join("astgrep_three.sh");
    let count = project::count(&m.projections[0], &fixtures(), ok.to_str().unwrap()).unwrap();
    assert_eq!(count, 3);
    assert_eq!(
        project::sentence(&m.projections[0], count),
        "`Instrument` is named at 3 sites."
    );
    assert_eq!(
        project::instrument_version(Instrument::AstGrep, ok.to_str().unwrap()).unwrap(),
        "ast-grep 0.44.1"
    );
}

/// Row 30: no projections ⇒ no ast-grep dependency and no ast-grep line — but the `rustc`
/// line is still there. The asymmetry is derived from one rule (an instrument is recorded iff
/// it produced a number), not special-cased, so it is asserted in both directions.
#[test]
fn no_proj() {
    let m = load("dogfood.toml");
    assert!(m.projections.is_empty());
    assert_eq!(block::used_instruments(&m), vec![Instrument::Toolchain]);
    let outcomes: Vec<Outcome> = m
        .probes
        .iter()
        .map(|p| Outcome {
            id: p.id.clone(),
            rc: 0,
            verdict: Verdict::Pass { mounts_reached: 1 },
        })
        .collect();
    let rendered = block::render(
        &m,
        "m.toml",
        "0123456789abcdef",
        &[(Instrument::Toolchain, "rustc 1.97.0".into())],
        &outcomes,
        &[],
    );
    assert!(!rendered.contains("instrument ast-grep"));
    assert!(rendered.contains("// instrument rustc = rustc 1.97.0"));
    // and `check` classifies such a block with no tool installed at all
    assert_eq!(block::check(&rendered, &rendered), CheckOutcome::Clean);
    assert!(matches!(
        block::check(&rendered, &rendered.replace("P0_control", "P0_ctl")),
        CheckOutcome::Drift {
            cause: DriftCause::Code,
            ..
        }
    ));
}

// ------------------------------------------------------------------ row 31

/// Row 31: the exit-code contract. An instrument failure is 2, not the anyhow idiom's 1;
/// drift is 1, not 2. The `run <compiled.toml>` arm aborts at step 1, so it costs zero
/// target runs. The end-to-end drift-is-1 arm is `isolation_e2e::drift` (a) — it needs a
/// full pipeline run, which needs the namespace.
#[test]
fn exit_codes() {
    let code = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_probe-pin"))
            .args(args)
            .output()
            .unwrap()
            .status
            .code()
            .unwrap()
    };
    assert_eq!(
        code(&["run", fixtures().join("compiled.toml").to_str().unwrap()]),
        2
    );
    assert_eq!(
        code(&["check", fixtures().join("compiled.toml").to_str().unwrap()]),
        2
    );
    assert_eq!(code(&["run", "/nonexistent/manifest.toml"]), 2);

    assert_eq!(block::exit_code(&CheckOutcome::Clean), 0);
    assert_eq!(
        block::exit_code(&CheckOutcome::Drift {
            cause: DriftCause::Code,
            diff: String::new(),
            numbers_moved: true
        }),
        1
    );
    assert_eq!(
        block::exit_code(&CheckOutcome::Drift {
            cause: DriftCause::Instrument(vec![InstrumentChange {
                instrument: Instrument::Toolchain,
                was: "a".into(),
                now: "b".into()
            }]),
            diff: String::new(),
            numbers_moved: false
        }),
        1
    );
}

/// Final review-impl, MINOR-6: the manifest path reaches the digest AND the BEGIN line, so an
/// absolute one stamps a machine-specific path into a committed artifact — the opposite of the
/// reproducibility the digest exists to provide, and the one manifest path key that was not
/// validated workspace-relative. The refusal lands at §7 step 2, so this arm costs zero target
/// runs; before it existed the same invocation ran the whole pipeline and stamped `/tmp/…`.
#[test]
fn manifest_outside_the_workspace_is_refused() {
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("dogfood.toml");
    std::fs::copy(fixtures().join("dogfood.toml"), &path).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_probe-pin"))
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.contains("is not inside the workspace root"), "{err}");
    // and the absolute path never reached a rendered block
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains(":BEGIN manifest=/"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The same comparison, from the other side: the refusal above must fire for a manifest that IS
/// outside the root, and must NOT fire for one that is inside it by a path whose components
/// include a symlink. `strip_prefix` between a canonicalized manifest path and an unresolved
/// root is a lexical comparison wearing a realpath's clothes, and its failure mode is a refusal
/// no manifest edit can satisfy.
///
/// DISCLOSURE: not reached through cargo today — `cargo locate-project --workspace` starts from
/// `getcwd`, which the kernel answers physically, so the root it prints is already resolved
/// (measured: a run through `/tmp/<link> -> the worktree` exited 0 with every row rendered).
/// `target::workspace_root` canonicalizes at the producer anyway, and this is that rule stated
/// where a test can drive it: arm 1 below is the escaping pair, and it is refused by the shape
/// this replaced.
#[test]
#[cfg_attr(
    not(unix),
    ignore = "creates a symlink; off Unix that needs Developer Mode (Windows) or has no API at all"
)]
fn manifest_rel_compares_realpaths() {
    let base = tempfile::tempdir().unwrap();
    let real = base.path().join("real");
    std::fs::create_dir_all(real.join("crates")).unwrap();
    std::fs::write(real.join("crates/m.toml"), "version = 1\n").unwrap();
    let link = base.path().join("link");
    common::symlink(&real, &link).unwrap();

    // arm 1 — the phenomenon: a canonicalized manifest path shares no prefix with a root
    // reached through a symlink, so the lexical comparison rejects a manifest that is inside it
    assert!(
        link.join("crates/m.toml")
            .canonicalize()
            .unwrap()
            .strip_prefix(&link)
            .is_err(),
        "the escaping pair must be lexically unrelated, or this arm is vacuous"
    );
    // the guard canonicalizes BOTH sides, so the same pair resolves — through the symlink and
    // through the real path alike
    assert_eq!(
        manifest::workspace_relative(&link.join("crates/m.toml"), &link).unwrap(),
        "crates/m.toml"
    );
    assert_eq!(
        manifest::workspace_relative(&real.join("crates/m.toml"), &real).unwrap(),
        "crates/m.toml"
    );

    // and a manifest that really is outside the root is still refused, by name
    let outside = base.path().join("outside.toml");
    std::fs::write(&outside, "version = 1\n").unwrap();
    let e = manifest::workspace_relative(&outside, &link).unwrap_err();
    assert!(
        e.to_string().contains("is not inside the workspace root"),
        "{e}"
    );
    // as is one that does not exist at all: `canonicalize` is the existence check too
    let e = manifest::workspace_relative(&real.join("crates/gone.toml"), &real).unwrap_err();
    assert!(
        e.to_string().contains("is not inside the workspace root"),
        "{e}"
    );
}

// ------------------------------------- §8 message fidelity on the two sentinel-free paths

/// §8's texts are the contract, and each of these rows promised a value the run never measured
/// (MINOR-2, MINOR-3): a rendered `-1` reads as an exit status when the process never started,
/// and `scratch_dir`'s failure named "the mutant" for a directory that holds none yet.
#[test]
fn abort_texts_promise_only_measured_values() {
    let spawn_failed = Abort::IsolationUnavailable {
        rc: None,
        stderr: "No such file or directory (os error 2)".into(),
    }
    .to_string();
    assert!(
        spawn_failed.contains("could not be spawned"),
        "{spawn_failed}"
    );
    assert!(!spawn_failed.contains("-1"), "no sentinel: {spawn_failed}");

    let ran = Abort::IsolationUnavailable {
        rc: Some(1),
        stderr: "unshare: write failed /proc/self/uid_map".into(),
    }
    .to_string();
    assert!(ran.contains("failed (exit 1):"), "{ran}");

    // the scratch-dir path names the DIRECTORY, and claims no mutant
    let e = isolate::scratch_dir(Path::new("/nonexistent-tmpdir"), Path::new("/")).unwrap_err();
    assert!(matches!(e, Abort::MaterializeFailed { .. }), "{e}");
    assert!(
        e.to_string()
            .starts_with("probe-pin: scratch-directory write failed at /nonexistent-tmpdir:"),
        "{e}"
    );
}

// ------------------------------------------------------------------ row 33

/// Row 33: a run that selected ZERO tests is an INSTRUMENT FAILURE, not a pass. Measured on
/// a real libtest selection: `filter_match = "exact"` against a module-prefix filter selects
/// nothing, and so does a rotted filter under a perfectly correct substring argv. The
/// ordering arm — the abort names the CONTROL and no probe runs — is
/// `isolation_e2e::no_tests_selected_ordering`.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "spawns GNU timeout(1); see docs/probe-pin.md"
)]
fn no_tests_selected() {
    let mut t = target();
    // arm 1: the shipping substring filter selects tests
    let run = child("ppfixture_census", &[], &[]);
    let obs = verdict::observe("P0_control", &run, &t, &[]).expect("substring filter selects");
    assert!(obs.test_count > 0 && !obs.target_failed);

    // arm 2: exact match on a filter that is a PREFIX selects nothing
    let run = child("ppfixture_cens", &[], &["--exact"]);
    t.filter = "ppfixture_cens".into();
    t.filter_match = manifest::FilterMatch::Exact;
    let e = verdict::observe("P0_control", &run, &t, &[]).unwrap_err();
    assert_eq!(run.rc, 0, "libtest reports a green exit for a 0-test run");
    assert!(
        matches!(e, Abort::NothingExecuted { ref probe, filter_match: manifest::FilterMatch::Exact, reason: NothingRan::NoneSelected, .. } if probe == "P0_control"),
        "{e}"
    );
    assert!(e.to_string().contains("INSTRUMENT FAILURE"));

    // arm 3: a filter that ROTTED (renamed upstream), under a correct substring argv
    let run = child("ppfixture_census_OLDNAME", &[], &[]);
    t.filter = "ppfixture_census_OLDNAME".into();
    t.filter_match = manifest::FilterMatch::Substring;
    assert!(matches!(
        verdict::observe("P0_control", &run, &t, &[]).unwrap_err(),
        Abort::NothingExecuted {
            reason: NothingRan::NoneSelected,
            ..
        }
    ));
}

/// The EXECUTION floor — the general guarantee of the class-closure pass, and the one gate that
/// covers a suppressing flag nobody enumerated.
///
/// `test_count` is what the OLD floor read, and it is IDENTICAL (1) across all three shapes
/// below: a normal run, an `#[ignore]`d pinned test, and a `--bench` argv. That equality is
/// asserted first, because it is the whole reason the floor had to move to a different field —
/// and because it is what makes the DROP arm (restore `test_count == 0`) flip both hostile arms
/// green while every other assertion in this file stays put.
///
/// The `#[ignore]` route needs NO manifest change at all, so digest, block and Tilt resource
/// stay green forever; `--bench` is reachable through `[target].args`, which is why the argv
/// shape check alone would not have closed it.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "spawns GNU timeout(1); see docs/probe-pin.md"
)]
fn execution_floor_reads_what_executed() {
    let mut t = target();
    let obs = |run: &Run, t: &Target| verdict::observe("P0_control", run, t, &[]);

    // the reach guard, and the reason the floor moved: SELECTION cannot tell these apart
    let normal = child("ppfixture_census", &[], &[]);
    let ignored = child("ppfixture_ignored", &[], &[]);
    let benched = child("ppfixture_census", &[], &["--bench"]);
    for (name, run) in [("ignored", &ignored), ("--bench", &benched)] {
        assert!(
            run.stdout.contains("\"test_count\": 1"),
            "{name}: libtest must still report test_count 1, or this test pins nothing: {}",
            run.stdout
        );
        assert_eq!(run.rc, 0, "{name}: libtest exits GREEN on a skipped run");
    }

    // positive: a run that executed a body is measured
    let ok = obs(&normal, &t).expect("a normal run must measure");
    assert_eq!((ok.passed, ok.failed, ok.ignored), (1, 0, 0));

    // hostile 1: the BLOCKER — `#[ignore]` on the pinned test
    t.filter = "ppfixture_ignored".into();
    let e = obs(&ignored, &t).unwrap_err();
    assert!(
        matches!(
            e,
            Abort::NothingExecuted {
                reason: NothingRan::AllSkipped,
                test_count: 1,
                ignored: 1,
                ..
            }
        ),
        "{e}"
    );
    assert!(e.to_string().contains("MEASURED NOTHING"), "{e}");
    assert!(
        e.to_string().contains("was SKIPPED"),
        "the message must distinguish skipped from unselected: {e}"
    );

    // hostile 2: `--bench`, which is deliberately NOT on any denylist
    t.filter = "ppfixture_census".into();
    t.args = vec!["--bench".into()];
    let e = obs(&benched, &t).unwrap_err();
    assert!(
        matches!(
            e,
            Abort::NothingExecuted {
                reason: NothingRan::AllSkipped,
                ..
            }
        ),
        "{e}"
    );

    // and the other parameter of the SAME abort still reads as a selection failure
    t.args.clear();
    t.filter = "ppfixture_census_OLDNAME".into();
    let e = obs(&child("ppfixture_census_OLDNAME", &[], &[]), &t).unwrap_err();
    assert!(e.to_string().contains("selected ZERO tests"), "{e}");
    assert!(!e.to_string().contains("was SKIPPED"), "{e}");

    // a FAILING run is executed too: the floor is on passed + failed, not on passed
    let failed = child("ppfixture_panic", &[("PP_FIXTURE", "panic")], &["--exact"]);
    t.filter = "ppfixture_panic".into();
    let obs = obs(&failed, &t).expect("a failing run measured something");
    assert_eq!((obs.passed, obs.failed), (0, 1));
}

// ------------------------------------------------------------------ argv + verdict wiring

/// `filter_match` and `[target].args` both reach libtest's own selection — the only place
/// where a dead schema field would show. Measured against this binary's real test names.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "spawns GNU timeout(1); see docs/probe-pin.md"
)]
fn argv_is_live() {
    let mut t = target();
    t.filter = "ppfixture".into();
    assert_eq!(
        isolate::argv(&t),
        vec![
            "ppfixture",
            "--test-threads=1",
            "--format",
            "json",
            "-Z",
            "unstable-options"
        ]
    );
    t.filter_match = manifest::FilterMatch::Exact;
    assert_eq!(isolate::argv(&t)[1], "--exact");
    t.filter_match = manifest::FilterMatch::Substring;
    t.args = vec!["--skip".into(), "ppfixture_census".into()];
    assert_eq!(isolate::argv(&t).last().unwrap(), "ppfixture_census");

    let all = child("ppfixture", &[], &[]);
    let skipped = child("ppfixture", &[], &["--skip", "ppfixture_census"]);
    let count = |r: &Run| {
        let mut t = target();
        t.filter = "ppfixture".into();
        verdict::observe("P", r, &t, &[]).unwrap().test_count
    };
    // `--exact` here would select 0 (the filter is a prefix), which is exactly row 33's arm 2
    assert!(
        count(&all) > count(&skipped),
        "[target].args must reach the argv"
    );
}

// ------------------------------------------------ surface 1: argv (the class, not the field)

/// The ARGV surface. `RESERVED` was checked against `[target].args` while `[target].filter` was
/// argv token 0 — where libtest's getopts parses it as a flag. The fix is not another field in
/// the loop: `Target::manifest_argv` is the single enumeration of what the manifest contributes,
/// `isolate::argv` BUILDS the argv from it, and validation checks the same list, so a future
/// argv-feeding field is covered by construction.
///
/// The DROP arm is `validate` iterating `m.target.args` again: arms 1 and 2 go green, arm 3
/// (the drift assertion) stays green — which is why arm 3 alone is not the test.
#[test]
fn argv_surface_is_one_authority() {
    let mut m = load("dogfood.toml");

    // arm 1 — a reserved flag in the FILTER slot is refused, and named as the filter
    m.target.filter = "--format".into();
    let e = abort_of(manifest::validate(&m).unwrap_err());
    assert!(
        matches!(e, Abort::ReservedArg { ref field, ref arg } if field == "[target].filter" && arg == "--format"),
        "{e}"
    );

    // arm 2 — the SHAPE constraint, stated as what an OPERAND is. `--bench` is on no denylist
    // (deliberately: §1's floor is what generalizes), so only the shape rule sees it here.
    for hostile in ["--bench", "-Zunstable-options", "--list", "-h"] {
        m.target.filter = hostile.into();
        let e = manifest::validate(&m).unwrap_err();
        assert!(
            e.to_string().contains("begins with '-'")
                || matches!(abort_of(e), Abort::ReservedArg { .. }),
            "{hostile} was accepted"
        );
    }
    // positive: an ordinary filter, and a hyphen INSIDE one, still validate
    for benign in ["ppfixture_census", "census_two-part"] {
        m.target.filter = benign.into();
        assert!(manifest::validate(&m).is_ok(), "{benign:?} was refused");
    }
    // `""` used to sit in that list, asserting a blank filter validates — which it no longer does.
    // Kept as a probe of WHICH guard refuses it: the naming rule, not this surface. If the argv
    // rule ever starts rejecting it, the benign list above stops proving the rule is discriminating
    // and this arm says so rather than passing on any refusal at all.
    m.target.filter = String::new();
    let e = manifest::validate(&m).unwrap_err();
    assert!(
        e.to_string().contains("[target].filter is blank"),
        "a blank filter must be refused for naming nothing: {e}"
    );
    assert!(
        !e.to_string().contains("begins with '-'"),
        "the leading-'-' operand rule must not be what rejects a blank filter: {e}"
    );

    // arm 2b — the SAME rule on the other program probe-pin hands manifest operands to. `-h`
    // there is not hypothetical: measured on the shipping binary, ast-grep printed its HELP and
    // probe-pin counted the lines into a rendered "named at 30 sites" sentence, exit 0.
    let mut p = load("projection.toml");
    assert_eq!(
        manifest::positional_argv_values(&p).len(),
        2,
        "the enumeration must cover [target].filter AND the projection's paths"
    );
    p.projections[0].paths = vec!["-h".into()];
    let e = manifest::validate(&p).unwrap_err();
    assert!(e.to_string().contains("begins with '-'"), "{e}");
    assert!(e.to_string().contains("instrument_sites"), "{e}");
    p.projections[0].paths = vec!["crates/probe-pin/src".into()];
    assert!(manifest::validate(&p).is_ok());

    // arm 3 — the drift assertion: the argv is BUILT from the enumeration validation checks.
    // Every manifest token reaches the argv, and every argv token that is not probe-pin's own
    // came from that enumeration — so a field cannot reach libtest unvalidated.
    let mut t = target();
    t.filter = "ppfixture_census".into();
    t.args = vec!["--skip".into(), "ppfixture_ignored".into()];
    t.filter_match = manifest::FilterMatch::Exact;
    let argv = isolate::argv(&t);
    let contributed: Vec<&str> = t.manifest_argv().iter().map(|(_, a)| *a).collect();
    assert_eq!(
        contributed,
        ["ppfixture_census", "--skip", "ppfixture_ignored"]
    );
    for token in &contributed {
        assert!(
            argv.iter().any(|a| a == token),
            "{token} never reached argv"
        );
    }
    for token in &argv {
        assert!(
            isolate::OWNED_FLAGS.contains(&token.as_str()) || contributed.contains(&token.as_str()),
            "{token} is on the argv and is neither probe-pin's nor the manifest's"
        );
    }
    // arm 4 — the ownership allowlist and the argv builder are ONE list: `--exact` is the
    // conditional head, `UNCONDITIONAL_FLAGS` is the tail `argv` emits verbatim. Respelling the
    // tail inline (as it was) lets a flag reach libtest that the allowlist does not name.
    assert_eq!(isolate::OWNED_FLAGS[0], "--exact");
    assert_eq!(isolate::UNCONDITIONAL_FLAGS, &isolate::OWNED_FLAGS[1..]);
    let emitted: Vec<&str> = argv
        .iter()
        .map(String::as_str)
        .filter(|a| isolate::UNCONDITIONAL_FLAGS.contains(a))
        .collect();
    assert_eq!(emitted, isolate::UNCONDITIONAL_FLAGS);
    // and the conditional one is still conditional
    t.filter_match = manifest::FilterMatch::Substring;
    assert!(!isolate::argv(&t).contains(&"--exact".to_string()));
}

// ------------------------------------------------------------------ surface 2: env

/// The ENV surface. `[target].env` was applied LAST with no key restriction, over a child that
/// is `unshare … bash -c SCRIPT` — so `PATH` resolved the `mount` and `cmp` the readback is made
/// of. Measured on the shipping binary: a manifest pointing `PATH` at stub `mount`/`cmp`
/// rendered `mount reached: 1 file(s)` at exit 0 for a probe whose mutant was never mounted.
///
/// Arm 2 is the anti-drift half, and the reason the refused set is not a hand-written list
/// compared against another hand-written list: it reads the keys the SHIPPING child command
/// actually sets, through `Command::get_envs`, and requires each to be refused.
#[test]
fn env_surface_refuses_probe_pin_owned_keys() {
    let mut m = load("dogfood.toml");

    // arm 1 — the measured attack, plus the rest of the owned set
    for key in [
        "PATH",
        "HOME",
        "PP_MOUNTS",
        "PP_MUTANT_0",
        "PP_TARGET_3",
        "TIMEOUT",
        "TESTBIN",
    ] {
        m.target.env = BTreeMap::from([(key.to_string(), "/hostile".to_string())]);
        let e = manifest::validate(&m).unwrap_err();
        assert!(e.to_string().contains(key), "{key} was accepted: {e}");
        assert!(
            e.to_string().contains("probe-pin itself sets"),
            "{key} -> {e}"
        );
    }
    // positive: the target's OWN variables, and the one documented overridable default
    m.target.env = BTreeMap::from([
        ("PP_FIXTURE".to_string(), "treewrite".to_string()),
        ("RUST_MIN_STACK".to_string(), "268435456".to_string()),
        ("RUST_BACKTRACE".to_string(), "1".to_string()),
    ]);
    assert!(manifest::validate(&m).is_ok());

    // arm 2 — every key the shipping child command sets must be refused by the same authority.
    // `[target].env` is emptied first, so what remains in `get_envs` is probe-pin's alone.
    let mut t = target();
    t.env.clear();
    let mounts = vec![
        mutate::Mount {
            mutant: PathBuf::from("/scratch/P1/a.rs"),
            target: PathBuf::from("/ws/a.rs"),
        },
        mutate::Mount {
            mutant: PathBuf::from("/scratch/P1/b.rs"),
            target: PathBuf::from("/ws/b.rs"),
        },
    ];
    let cmd = isolate::child_command(isolate::SCRIPT, Path::new("/bin/true"), &mounts, &t);
    let keys: Vec<String> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_some())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    assert!(
        keys.iter().any(|k| k == "PP_MUTANT_1") && keys.iter().any(|k| k == "PATH"),
        "instrument check: the readback must see the real key set, got {keys:?}"
    );
    for key in &keys {
        assert!(
            isolate::is_probe_pin_owned_env(key) || key == "RUST_MIN_STACK",
            "the child sets {key}, which [target].env may still override. Either add it to \
             isolate's owned set or document it like RUST_MIN_STACK. keys: {keys:?}"
        );
    }
}

// ------------------------------------------------------------------ surface 3: filesystem path

/// The PATH surface. `is_workspace_relative` is LEXICAL — every component `Normal` says nothing
/// about what a component RESOLVES to — so an in-tree symlink satisfies it and lands the write
/// anywhere on the filesystem (measured on the shipping binary: `[output].file` through one,
/// exit 0, the block spliced outside the repository).
///
/// Arm 1 is the phenomenon: the lexical check ACCEPTS the escaping key, which is what makes the
/// containment assertion non-vacuous. `isolate::scratch_dir`'s canonicalize-then-`starts_with`
/// is the in-tree pattern this reuses, in both directions — into the workspace here, and into
/// the scratch dir in `mutant_destination_cannot_escape_the_scratch_dir`.
#[test]
#[cfg_attr(
    not(unix),
    ignore = "creates symlinks; off Unix that needs Developer Mode (Windows) or has no API at all"
)]
fn path_surface_is_realpath_containment() {
    let base = tempfile::tempdir().unwrap();
    let ws = base.path().join("ws");
    let outside = base.path().join("outside");
    std::fs::create_dir_all(ws.join("crates")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("victim.rs"), "OUTSIDE\n").unwrap();
    common::symlink(&outside, ws.join("crates/link")).unwrap();
    common::symlink(outside.join("does_not_exist"), ws.join("crates/dangling")).unwrap();

    // arm 1 — the phenomenon: every escaping key below passes the LEXICAL check
    for key in [
        "crates/link/victim.rs",
        "crates/link/new_file.md",
        "crates/dangling",
    ] {
        assert!(
            manifest::is_workspace_relative(key),
            "{key} must be lexically clean, or the containment arm is vacuous"
        );
        let why =
            manifest::resolve_contained(&ws, key).expect_err(&format!("{key} escaped containment"));
        assert!(
            why.contains("outside") || why.contains("symlink whose target does not exist"),
            "{key} -> {why}"
        );
    }
    // and the lexical refusals still read as lexical ones
    for key in ["../escape.md", "/tmp/escape.md", "crates/../../escape.md"] {
        let why = manifest::resolve_contained(&ws, key).unwrap_err();
        assert!(why.contains("not a relative path"), "{key} -> {why}");
    }

    // positive: an existing in-tree file, a file that does not exist yet, and a nested new path
    std::fs::write(ws.join("crates/real.rs"), "IN\n").unwrap();
    for key in ["crates/real.rs", "crates/new.md", "crates/deep/new.md"] {
        assert!(
            manifest::resolve_contained(&ws, key).is_ok(),
            "{key} was refused"
        );
    }

    // and it is WIRED: validate_paths refuses the same key on both path fields
    let m = |output: &str, mutated: &str| -> Manifest {
        toml::from_str(&format!(
            "version = 1\n\
             [target]\nmode='runtime-read'\npackage='p'\ntest='t'\nfilter='f'\nfilter_match='substring'\n\
             [output]\nfile={output:?}\nmarker='PROBE-PIN'\n\
             [[probe]]\nid='P0_control'\n[probe.expect]\noutcome='pass'\n\
             [[probe]]\nid='P1'\n[[probe.mutation]]\nkind='replace'\nfile={mutated:?}\nfind='a'\nreplace='b'\n\
             [probe.expect]\noutcome='pass'\n"
        ))
        .expect("manifest parses")
    };
    let ok = m("crates/new.md", "crates/real.rs");
    manifest::validate(&ok).expect("the benign manifest validates");
    manifest::validate_paths(&ok, &ws).expect("the benign manifest is contained");

    let escaping_output = m("crates/link/new_file.md", "crates/real.rs");
    manifest::validate(&escaping_output).expect("the LEXICAL pass is the phenomenon");
    let e = manifest::validate_paths(&escaping_output, &ws).unwrap_err();
    assert!(e.to_string().contains("[output].file"), "{e}");
    assert!(e.to_string().contains("is not workspace-relative"), "{e}");

    let escaping_mutation = m("crates/new.md", "crates/link/victim.rs");
    manifest::validate(&escaping_mutation).expect("the LEXICAL pass is the phenomenon");
    let e = manifest::validate_paths(&escaping_mutation, &ws).unwrap_err();
    assert!(e.to_string().contains("P1 names"), "{e}");

    // the fourth path producer: a projection's operands reach ast-grep, in the same tree
    let mut projecting = load("projection.toml");
    projecting.projections[0].paths = vec!["crates/link".into()];
    let e = manifest::validate_paths(&projecting, &ws).unwrap_err();
    assert!(e.to_string().contains("instrument_sites"), "{e}");
    assert!(e.to_string().contains("scans"), "{e}");
    projecting.projections[0].paths = vec!["crates/real.rs".into()];
    assert!(manifest::validate_paths(&projecting, &ws).is_ok());
}

/// The same containment rule on the WRITE side, where the base is the scratch dir. `mutate::apply`
/// writes `scratch.join(probe.id).join(rel)`: both halves are lexically constrained already, and
/// neither says what the join resolves to. Arm 1 exhibits the escape — a symlink in the scratch
/// dir named as a plain probe id — writing a mutant straight into the measured workspace.
#[test]
#[cfg_attr(
    not(unix),
    ignore = "creates a symlink; off Unix that needs Developer Mode (Windows) or has no API at all"
)]
fn mutant_destination_cannot_escape_the_scratch_dir() {
    let base = tempfile::tempdir().unwrap();
    let ws = base.path().join("ws");
    let scratch = base.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    write(&ws, "f.txt", "alpha\n");
    let p = probe(
        "id='P1_plain_name'\n[[mutation]]\nkind='replace'\nfile='f.txt'\nfind='alpha'\nreplace='beta'\n\
         [expect]\noutcome='pass'\n",
    );
    // the id is a plain name and the file is relative — both LEXICAL checks pass
    assert!(manifest::is_plain_name(&p.id) && manifest::is_workspace_relative("f.txt"));

    // arm 1 — the phenomenon, with the guard's own base symlinked into the workspace
    common::symlink(&ws, scratch.join("P1_plain_name")).unwrap();
    let e = mutate::apply(&p, &ws, &scratch).unwrap_err();
    assert!(
        e.to_string()
            .contains("outside probe-pin's scratch directory"),
        "{e}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("f.txt")).unwrap(),
        "alpha\n",
        "nothing may be written into the measured tree"
    );

    // positive: an ordinary scratch dir still materializes the mutant
    std::fs::remove_file(scratch.join("P1_plain_name")).unwrap();
    let mounts = mutate::apply(&p, &ws, &scratch).expect("the shipping shape must survive");
    assert_eq!(mounts.len(), 1);
    assert_eq!(
        std::fs::read_to_string(scratch.join("P1_plain_name/f.txt")).unwrap(),
        "beta\n"
    );
}

// ------------------------------------------------------------------ surface 4: anchor semantics

/// The ANCHOR surface. The empty anchor LIST was refused because "any failure would satisfy it";
/// `all_anchors_in_one` matches with `contains`, and `contains("")` is true for every capture —
/// so `anchor = [""]` is that identical hazard one character away, and it was accepted
/// (measured: exit 0, a green `fail` row with a BLANK firing-assertion cell, which reads as "no
/// anchor was needed").
#[test]
fn anchor_surface_refuses_a_blank_anchor() {
    // the phenomenon: a blank anchor is satisfied by any capture at all
    assert!(verdict::all_anchors_in_one(
        &[String::new()],
        &["a totally unrelated failure".to_string()]
    ));
    assert!(!verdict::all_anchors_in_one(
        &["REAL".to_string()],
        &["a totally unrelated failure".to_string()]
    ));

    let mut m = load("dogfood.toml");
    for hostile in [vec![""], vec![" "], vec!["\t\n"], vec!["REAL ANCHOR", ""]] {
        m.probes[1].expect = probe_pin::manifest::Expect::Fail {
            anchor: hostile.iter().map(|s| (*s).to_string()).collect(),
        };
        let e = manifest::validate(&m).unwrap_err();
        assert!(
            e.to_string().contains("empty after trimming"),
            "{hostile:?} was accepted: {e}"
        );
        assert!(e.to_string().contains("P1_drop_second_site"), "{e}");
    }
    // the empty LIST still reports as the empty list, not as a blank anchor
    m.probes[1].expect = probe_pin::manifest::Expect::Fail { anchor: vec![] };
    let e = manifest::validate(&m).unwrap_err();
    assert!(e.to_string().contains("empty anchor list"), "{e}");
    // positive: the shipping anchors validate
    assert!(manifest::validate(&load("dogfood.toml")).is_ok());
}

// ------------------------------------------------- surface 5: the text the block is made of

/// Replace every string LEAF of a serialized manifest with a unique sentinel, so the census
/// below measures the struct's real string surface instead of a hand-kept list. The four keys
/// skipped are serde's enum TAGS (`mode`, `filter_match`, `kind`, `outcome`) — sentinelling one
/// makes the value fail to deserialize, which is what the round-trip `expect` reports.
fn sentinel_strings(v: &mut serde_json::Value, key: &str, out: &mut Vec<String>) {
    const TAG_KEYS: &[&str] = &["mode", "filter_match", "kind", "outcome"];
    match v {
        serde_json::Value::String(s) if !TAG_KEYS.contains(&key) => {
            *s = format!("PPSENT{:03}_{key}", out.len());
            out.push(s.clone());
        }
        serde_json::Value::Array(items) => {
            for item in items {
                sentinel_strings(item, key, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (k, value) in map.iter_mut() {
                let k = k.clone();
                sentinel_strings(value, &k, out);
            }
        }
        _ => {}
    }
}

/// The BLOCK-TEXT surface, with a CENSUS of its producers.
///
/// The block is a line-oriented, marker-delimited artifact — `block::locate` counts
/// `<marker>:BEGIN`/`<marker>:END` per LINE — and `cell()` escapes only `|`. Measured on the
/// shipping binary: `anchor = ["PROBE-PIN:END"]` rendered into the firing-assertion cell,
/// `run --write` exited 0, and the spliced file then carried TWO END markers, after which every
/// `check` aborts `MarkerNotUnique` (found 1/2). The pin could only be recovered by hand.
///
/// Arm 1 is the census and the reason this is a SURFACE and not four field checks: it
/// sentinels every string in the manifest through serde, renders a block, and asserts that the
/// set of sentinels that REACH the block is exactly the set `rendered_block_values` enumerates.
/// A future field that starts rendering is either in the enumeration or fails this test —
/// which is the property the argv surface got after `[target].filter` was found reaching
/// libtest with only `[target].args` validated.
#[test]
fn block_text_surface_is_one_authority() {
    // ---- arm 1: the census
    let mut m = load("dogfood.toml");
    m.projections = load("projection.toml").projections;
    let mut json = serde_json::to_value(&m).expect("manifest serializes");
    let mut sentinels: Vec<String> = Vec::new();
    sentinel_strings(&mut json, "", &mut sentinels);
    let m: Manifest = serde_json::from_value(json).expect(
        "the sentinelled manifest must round-trip. If this failed, a new serde enum TAG field \
         exists and must be added to TAG_KEYS in sentinel_strings",
    );
    assert!(
        sentinels.len() > 12,
        "instrument check: the walk must find the manifest's strings, found {}",
        sentinels.len()
    );
    // `manifest_rel` is not a manifest value, but it is a caller string stamped on the BEGIN
    // line, so it belongs to this surface and carries a sentinel like everything else.
    let manifest_rel = "PPSENT999_manifest_rel";
    let outcomes: Vec<Outcome> = m
        .probes
        .iter()
        .map(|p| Outcome {
            id: p.id.clone(),
            rc: 101,
            verdict: match &p.expect {
                probe_pin::manifest::Expect::Pass {} => Verdict::Pass { mounts_reached: 1 },
                // a Fail verdict is what renders the anchors; without one they are off-surface
                probe_pin::manifest::Expect::Fail { .. } => Verdict::Fail {
                    provenance: Vec::new(),
                },
            },
        })
        .collect();
    let counts: Vec<ProjectionCount> = m
        .projections
        .iter()
        .map(|p| ProjectionCount {
            id: p.id.clone(),
            count: 3,
        })
        .collect();
    let rendered = block::render(&m, manifest_rel, "d1g357", &[], &outcomes, &counts);

    let appearing: Vec<&str> = sentinels
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(manifest_rel))
        .filter(|s| rendered.contains(*s))
        .collect();
    let mut covered: Vec<&str> = manifest::rendered_block_values(&m, manifest_rel)
        .into_iter()
        .map(|(_, value)| value)
        .collect();
    covered.sort_unstable();
    covered.dedup();
    let mut appearing_sorted = appearing.clone();
    appearing_sorted.sort_unstable();
    assert_eq!(
        appearing_sorted, covered,
        "the block-text census moved. Every manifest string that reaches the rendered block \
         must be in `manifest::rendered_block_values`, which is what `validate_block_text` \
         refuses control characters and marker tags in. rendered:\n{rendered}"
    );
    // instrument check: the census must be able to see a value that is NOT rendered, or a
    // pass proves nothing about the enumeration's tightness
    assert!(
        sentinels.len() > appearing.len(),
        "instrument check: some manifest strings ([target].package, [output].file, claim, \
         assert_count.file) are NOT rendered; if all of them appear, this census cannot fail"
    );

    // ---- arm 2: every producer on the surface refuses the same hostile shapes
    let mut base = load("dogfood.toml");
    base.projections = load("projection.toml").projections;
    let rel = "crates/probe-pin/tests/fixtures/dogfood.toml";
    manifest::validate_block_text(&base, rel).expect("the shipping manifests render a safe block");

    for hostile in [
        "PROBE-PIN:END",
        "PROBE-PIN:BEGIN",
        "two\nlines",
        "nul\0byte",
    ] {
        let mut m = base.clone();
        m.probes[1].expect = probe_pin::manifest::Expect::Fail {
            anchor: vec![hostile.to_string()],
        };
        let e = manifest::validate_block_text(&m, rel).expect_err(&format!(
            "the anchor {hostile:?} was ACCEPTED into the block"
        ));
        assert!(e.to_string().contains("anchor"), "anchor {hostile:?}: {e}");

        let mut m = base.clone();
        m.projections[0].sentence = format!("{hostile} {{count}}");
        let e = manifest::validate_block_text(&m, rel).expect_err(&format!(
            "the sentence {hostile:?} was ACCEPTED into the block"
        ));
        assert!(
            e.to_string().contains("sentence"),
            "sentence {hostile:?}: {e}"
        );

        let mut m = base.clone();
        m.probes[1].mutations = vec![probe_pin::manifest::Mutation::Replace {
            file: format!("crates/{hostile}/x.rs"),
            find: "a".into(),
            replace: "b".into(),
        }];
        let e = manifest::validate_block_text(&m, rel).expect_err(&format!(
            "the mutation path {hostile:?} was ACCEPTED into the block"
        ));
        assert!(
            e.to_string().contains("mutation file"),
            "mutation file {hostile:?}: {e}"
        );

        let e = manifest::validate_block_text(&base, &format!("crates/{hostile}/m.toml"))
            .expect_err(&format!(
                "the manifest path {hostile:?} was ACCEPTED into the block"
            ));
        assert!(
            e.to_string().contains("the manifest path"),
            "manifest path {hostile:?}: {e}"
        );
    }

    // ---- arm 3: the marker itself, which is the structure the rest is checked against
    for hostile in ["", " ", "\t", "PROBE PIN", "PROBE-PIN:END"] {
        let mut m = base.clone();
        m.output.marker = hostile.to_string();
        let e = manifest::validate_block_text(&m, rel).expect_err(&format!(
            "the marker {hostile:?} was ACCEPTED as the block's tag"
        ));
        assert!(
            e.to_string().contains("is not a plain name"),
            "marker {hostile:?} was accepted: {e}"
        );
    }
    // positive: the shipping marker, and an ordinary alternative one, both validate
    for benign in ["PROBE-PIN", "PIN_2"] {
        let mut m = base.clone();
        m.output.marker = benign.to_string();
        assert!(
            manifest::validate_block_text(&m, rel).is_ok(),
            "{benign} was refused"
        );
    }
    // positive: a real anchor with punctuation, quotes and a colon still validates — the
    // refusal is about control characters and the marker, not about anchor text being rich
    let mut m = base.clone();
    m.probes[1].expect = probe_pin::manifest::Expect::Fail {
        anchor: vec!["CENSUS VIOLATED: expected 2 sites, got 1".into()],
    };
    assert!(manifest::validate_block_text(&m, rel).is_ok());
}

/// The `[[projection]]` producer checks. A projection is a block-row producer exactly like a
/// probe, and it had only its path keys checked. All three refusals below were measured on the
/// shipping binary rendering a block at exit 0.
#[test]
fn projection_surface_gets_the_producer_checks_its_siblings_get() {
    let base = load("projection.toml");
    manifest::validate(&base).expect("the shipping projection fixture validates");

    // duplicate id: counts are looked up BY ID, so the second sentence renders the first
    // projection's number (measured: a 1-match pattern rendered as "7 sites")
    let mut m = base.clone();
    let mut twin = base.projections[0].clone();
    twin.pattern = "Abort::$V".into();
    twin.sentence = "`Abort` is named at {count} sites.".into();
    m.projections.push(twin);
    let e = manifest::validate(&m).expect_err("a duplicate projection id was ACCEPTED");
    assert!(e.to_string().contains("duplicate"), "{e}");
    assert!(e.to_string().contains("instrument_sites"), "{e}");
    // the phenomenon itself, so the refusal is not merely stylistic: with two ids equal, the
    // render of the SECOND projection carries the FIRST one's count
    let counts = vec![ProjectionCount {
        id: "instrument_sites".into(),
        count: 7,
    }];
    let rendered = block::render(&m, "m.toml", "d", &[], &[], &counts);
    assert_eq!(
        rendered.matches("at 7 sites").count(),
        2,
        "the duplicate id must render the same number twice:\n{rendered}"
    );

    // a non-plain id, exactly as a probe id is refused
    let mut m = base.clone();
    m.projections[0].id = "no placeholder".into();
    let e = manifest::validate(&m).expect_err("a non-plain projection id was ACCEPTED");
    assert!(e.to_string().contains("is not a plain name"), "{e}");

    // an empty path list: ast-grep with no operand scans the whole cwd, so the rendered number
    // is a measurement of a tree the manifest never named (measured: 15 against the meant 7)
    let mut m = base.clone();
    m.projections[0].paths = Vec::new();
    let e = manifest::validate(&m).expect_err("an empty projection path list was ACCEPTED");
    assert!(e.to_string().contains("names no path"), "{e}");

    // a sentence with no substitution point renders a complete-looking claim with no number
    let mut m = base.clone();
    m.projections[0].sentence = "`Instrument` is named at every site that needs one.".into();
    let e = manifest::validate(&m)
        .expect_err("a sentence with no {count} placeholder was ACCEPTED into the block");
    assert!(e.to_string().contains("{count}"), "{e}");
    assert_eq!(
        project::sentence(&m.projections[0], 7),
        "`Instrument` is named at every site that needs one.",
        "the phenomenon: the count is substituted nowhere, and the sentence still renders"
    );

    // positive: the shipping projection, and one with the placeholder anywhere in the sentence
    let mut m = base.clone();
    m.projections[0].sentence = "{count} sites name `Instrument`.".into();
    assert!(manifest::validate(&m).is_ok());
}

/// The mutant's SIZE is a manifest-controlled product, and `Prepend` is the only unbounded one.
/// Measured on the shipping binary: `repeat = 4294967295` of a 14-byte pad asked for
/// 60,129,542,130 bytes, and an allocation failure is not a catchable panic — probe-pin died
/// with SIGABRT (exit 134, core dumped), outside its documented 0/1/2/101 exit contract and
/// after a full control run had already been spent.
#[test]
fn prepend_pad_is_bounded() {
    let mut m = load("dogfood.toml");
    let probe_pin::manifest::Mutation::Prepend { text, repeat, .. } = &mut m.probes[2].mutations[0]
    else {
        panic!("dogfood.toml's P2 is the Prepend probe; the fixture moved");
    };
    assert_eq!(text.len(), 14, "the shipping pad is 14 bytes");
    *repeat = u32::MAX;
    let e = manifest::validate(&m)
        .expect_err("a 60 GB pad was ACCEPTED — the run dies with SIGABRT, not a refusal");
    assert!(e.to_string().contains("60129542130 bytes"), "{e}");
    assert!(e.to_string().contains("P2_pad_reaches_target"), "{e}");

    // the boundary, from both sides, on the exact cap
    let cap = probe_pin::manifest::MAX_PREPEND_BYTES;
    let probe_pin::manifest::Mutation::Prepend { text, repeat, .. } = &mut m.probes[2].mutations[0]
    else {
        unreachable!()
    };
    *text = "x".to_string();
    *repeat = cap as u32;
    assert!(
        manifest::validate(&m).is_ok(),
        "the cap itself must be allowed"
    );
    let probe_pin::manifest::Mutation::Prepend { repeat, .. } = &mut m.probes[2].mutations[0]
    else {
        unreachable!()
    };
    *repeat = cap as u32 + 1;
    assert!(manifest::validate(&m).is_err(), "one byte over must refuse");

    // positive: the shipping 200 × 14 B pad is four orders of magnitude under the cap
    assert!(manifest::validate(&load("dogfood.toml")).is_ok());
}

/// Fixture manifests may not name a COMMITTED block file. `projection.toml` is driven with
/// `run --write`, and measured on the shipping binary with ast-grep installed it exited 0 and
/// replaced the committed dogfood block — the one `dogfood.toml`, `isolation_e2e::dogfood_check`
/// and the Tilt `probe-pin-check` resource all validate — with a projection block of its own.
///
/// Written as a census over the whole fixture directory rather than as an edit to the two
/// manifests that had the property: the risk belongs to "a fixture the binary is DRIVEN on",
/// not to those two files, and a new fixture gets the rule for free. The census found two more
/// than the review named — and it also found the honest exception: `p7.toml`/`p8.toml` are
/// archived corpus manifests, verbatim in shape (`schema_adequacy`), parsed and validated but
/// never handed to the binary, so what they name is an artifact of the archive.
#[test]
fn a_driven_fixture_never_names_a_committed_block() {
    const TMP: &str = "crates/probe-pin/tests/fixtures/tmp/";
    // The one file that runs the binary: `probe_pin_bin` is defined there and nowhere else, so
    // "named in it" IS "driven through the real pipeline".
    let e2e = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/isolation_e2e.rs"),
    )
    .expect("the Tier-2 suite is readable");
    assert_eq!(
        e2e.matches("fn probe_pin_bin(").count(),
        1,
        "instrument check: the binary is driven from exactly one helper, in this file. If it \
         moved, this census is reading the wrong source."
    );

    let (mut checked, mut driven) = (0, 0);
    for entry in std::fs::read_dir(fixtures()).expect("fixtures/ readable") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let m = Manifest::load(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        checked += 1;
        if !e2e.contains(&name) {
            continue; // archived corpus: parsed and validated, never run
        }
        driven += 1;
        if name == "dogfood.toml" {
            assert_eq!(
                m.output.file, "crates/probe-pin/tests/fixtures/dogfood_block.md",
                "the dogfood manifest is the one that owns the committed block"
            );
        } else {
            assert!(
                m.output.file.starts_with(TMP),
                "{name} is driven through the real binary and names [output].file = {:?}, which \
                 is not under the gitignored {TMP}. Measured: `run --write` on projection.toml \
                 exited 0 and replaced the committed dogfood block. Where a manifest happens to \
                 abort is not protection — it moves.",
                m.output.file
            );
        }
    }
    assert!(
        checked >= 8 && driven >= 5,
        "instrument check: {checked} fixture manifests, {driven} driven. If nothing is driven, \
         this census cannot fail."
    );
}

// ------------------------------------------------------- assert_count says what it measured

/// `assert_count` is evaluated against the MUTANT text, so a file the probe does not mutate had
/// no mutant — the old fallback read the PRISTINE tree and reported it as "in the MUTANT of".
/// Refusing is the half that cannot rot: the message stays true because the case is gone.
#[test]
fn assert_count_must_name_a_file_this_probe_mutates() {
    let text = std::fs::read_to_string(fixtures().join("dogfood.toml")).unwrap();
    let unmutated = text.replace(
        "  [[probe.assert_count]]\n  file  = \"crates/probe-pin/tests/fixtures/prod.txt\"",
        "  [[probe.assert_count]]\n  file  = \"crates/probe-pin/tests/fixtures/dogfood.toml\"",
    );
    assert_ne!(
        unmutated, text,
        "the fixture edit must apply, or this arm is vacuous"
    );
    let m = toml::from_str::<Manifest>(&unmutated).expect("manifest parses");
    assert_eq!(
        m.probes[1].assert_counts[0].file,
        "crates/probe-pin/tests/fixtures/dogfood.toml"
    );
    let e = manifest::validate(&m).unwrap_err();
    assert!(
        e.to_string().contains("which this probe does not mutate"),
        "{e}"
    );
    assert!(e.to_string().contains("P1_drop_second_site"), "{e}");

    // positive: the shipping assert_count names the file its probe mutates
    assert!(manifest::validate(&toml::from_str::<Manifest>(&text).unwrap()).is_ok());
}

/// `Verdict::Fail` IS "target failed AND every anchor in ONE record". Its complement is a
/// verdict MISMATCH naming the missed anchor — never a `Pass`, whose rendering would
/// transcribe "visible-and-still-passing" for a run whose target failed.
#[test]
fn fail_complement_is_a_mismatch() {
    let p = probe(
        "id='P1'\n[[mutation]]\nkind='replace'\nfile='f'\nfind='a'\nreplace='b'\n\
         [expect]\noutcome='fail'\nanchor=['HIT','MISS']\n",
    );
    let mut obs = verdict::Observed {
        rc: 101,
        test_count: 1,
        target_failed: true,
        failed_captures: vec!["panicked at tests/c.rs:1:1:\nHIT\n".into()],
        ..Default::default()
    };
    // the mismatch is TYPED: `main` formats it, and the missed anchor is a carried value, not
    // a substring of a pre-formatted report (MINOR-7)
    let e = verdict::verdict(&p, &obs, 1).unwrap_err();
    assert_eq!(e.missed_anchor.as_deref(), Some("MISS"));
    assert_eq!(e.observed_rc, 101);
    assert!(e.to_string().contains("anchor \"MISS\""), "{e}");

    obs.failed_captures[0].push_str("MISS\n");
    assert_eq!(
        verdict::verdict(&p, &obs, 1),
        Ok(Verdict::Fail {
            provenance: vec![PathBuf::from("tests/c.rs")]
        })
    );
}

// ------------------------------------------------------------------ targets, not assertions

const PROD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/prod.txt");

/// The runtime-read census the Tier-2 manifests mutate. Reading at RUNTIME is what makes a
/// bind-mounted mutant visible; an `include_str!` copy would be baked in at build time.
#[test]
fn ppfixture_census() {
    let text = std::fs::read_to_string(PROD).expect("prod.txt");
    let sites = text.lines().filter(|l| l.contains("marker")).count();
    assert_eq!(
        sites, 2,
        "CENSUS VIOLATED: expected 2 sites, got {sites}; text: {text:?}"
    );
}

/// The execution floor's fixture: a pinned test whose body NEVER RUNS. `#[ignore]` needs no
/// manifest change at all, so the digest, the block and the Tilt resource stay green forever —
/// and libtest still reports `test_count: 1` for it, which is why a floor written on
/// `test_count` cannot see it. The terminal record's `passed`/`failed`/`ignored` can.
#[test]
#[ignore = "a probe TARGET, not an assertion: driven by the execution-floor manifests"]
fn ppfixture_ignored() {
    assert!(std::fs::read_to_string(PROD).is_ok(), "prod.txt");
}

/// Row 8's fixture A: UNCONDITIONAL — no `RUST_MIN_STACK` value rescues it.
#[test]
fn ppfixture_abort() {
    if std::env::var("PP_FIXTURE").as_deref() == Ok("abort") {
        std::process::abort();
    }
}

/// Row 13's fixture B: RESCUABLE — 1200 × 4 KiB frames on a spawned thread, whose stack size
/// is what `RUST_MIN_STACK` sets.
#[test]
fn ppfixture_stack() {
    if std::env::var("PP_FIXTURE").as_deref() != Ok("stack") {
        return;
    }
    fn burn(n: u32) -> u64 {
        let frame = [0u8; 4096];
        if n == 0 {
            return std::hint::black_box(&frame)[0] as u64;
        }
        burn(n - 1) + std::hint::black_box(&frame)[7] as u64
    }
    std::thread::spawn(|| std::hint::black_box(burn(1200)))
        .join()
        .unwrap();
}

/// Row 12's fixture: a panic whose capture length depends on `RUST_BACKTRACE`.
#[test]
fn ppfixture_panic() {
    if std::env::var("PP_FIXTURE").as_deref() == Ok("panic") {
        panic!("PPFIXTURE PANIC");
    }
}

/// Row 12's instrument: a process whose ambient environment the caller owns, which runs
/// `ppfixture_panic` through the SHIPPING `apply_child_env` and reports the capture length.
#[test]
fn ppfixture_envprobe() {
    if std::env::var("PP_FIXTURE").as_deref() != Ok("envprobe") {
        return;
    }
    let mut t = target();
    t.env.clear();
    if std::env::var("PP_FORCE_BACKTRACE").is_ok() {
        t.env.insert("RUST_BACKTRACE".into(), "1".into());
    }
    let mut cmd = Command::new("timeout");
    cmd.args(["-k", "5", "60"])
        .arg(std::env::current_exe().unwrap())
        .args(["ppfixture_panic", "--exact", "--test-threads=1"])
        .args(["--format", "json", "-Z", "unstable-options"]);
    isolate::apply_child_env(&mut cmd, &t);
    cmd.env("PP_FIXTURE", "panic");
    let out = cmd.output().unwrap();
    let run = Run {
        rc: isolate::exit_rc(&out.status),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::new(),
    };
    let obs = verdict::observe("envprobe", &run, &t, &[]).unwrap();
    std::fs::write(
        std::env::var("PP_OUT").unwrap(),
        obs.failed_captures[0].len().to_string(),
    )
    .unwrap();
}

/// §7 step 10's fixture (MAJOR-2): a target that WRITES THE TREE. `unshare --mount` isolates
/// the mount table, not the filesystem, so this write reaches the real file whenever the path
/// is not mounted — which is exactly the control run. The appended line deliberately avoids
/// the word the manifest's `find` matches, so the write does not rot the probe it feeds.
#[test]
fn ppfixture_treewrite() {
    if std::env::var("PP_FIXTURE").as_deref() != Ok("treewrite") {
        return;
    }
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/tmp/tree_write.txt"
    );
    let text = std::fs::read_to_string(path).expect("tree_write.txt");
    std::fs::write(path, format!("{text}the target wrote here\n")).unwrap();
}

/// Row 14's fixture: hangs until `timeout` kills it.
#[test]
fn ppfixture_hang() {
    if std::env::var("PP_FIXTURE").as_deref() == Ok("hang") {
        std::thread::sleep(std::time::Duration::from_secs(600));
    }
}
