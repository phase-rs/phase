//! §7 steps 8-9: libtest's JSON records in, an ordered instrument-failure classification, a
//! verdict, and the cross-probe discrimination scan.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::isolate::Run;
use crate::manifest::{Expect, Probe, Target};
use crate::mutate::Mount;
use crate::{Abort, HarnessMarker, NothingRan};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass { mounts_reached: usize },
    Fail { provenance: Vec<PathBuf> },
}

/// What the record stream said, once no instrument failure fired.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    pub rc: i32,
    /// What the suite-STARTED record said it would run. Selection, not execution.
    pub test_count: usize,
    /// What the suite-TERMINAL record said actually happened. `passed + failed` is the only
    /// number in the stream that counts test BODIES that ran: `test_count` is identical for a
    /// normal run, an `#[ignore]`d run and a `--bench` run (measured, all three).
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
    pub filtered_out: usize,
    /// The `stdout` field of every `{"type":"test"}` `event:"failed"` record. libtest's
    /// per-test capture buffer: that test's `println!`, its `eprintln!` and its panic
    /// message, and NOTHING from any other test. Scoping is structural, not inferred.
    pub failed_captures: Vec<String>,
    pub target_failed: bool,
}

/// Classification is ORDERED, not a set: reserved script exit codes first, then stream shape,
/// then verdict. A timeout, an unreached mount and a script that failed before `exec` all
/// satisfy a `HarnessIncomplete` condition too — every one of them produces empty stdout and a
/// non-zero code — and `HarnessIncomplete`'s `RUST_MIN_STACK` hint is actively misleading for
/// all three. The script raises a RESERVED code for the two it can name (97 mount readback,
/// 96 script-level), so the cause stays typed instead of being inferred from a stderr prefix.
pub fn observe(
    probe: &str,
    run: &Run,
    target: &Target,
    mounts: &[Mount],
) -> Result<Observed, Abort> {
    // `97` is the mount signal only when stdout is empty: a reached mount always emits the
    // suite-started record first.
    if run.rc == 97 && run.stdout.trim().is_empty() {
        let path = run
            .stderr
            .split_once("MOUNT NOT REACHED: ")
            .and_then(|(_, rest)| rest.split_once(" (mutant"))
            .map(|(p, _)| PathBuf::from(p))
            .or_else(|| mounts.first().map(|m| m.target.clone()))
            .unwrap_or_default();
        return Err(Abort::MountNotReached {
            probe: probe.to_string(),
            path,
            stderr: run.stderr.trim().to_string(),
        });
    }
    // Same guard as 97, for the same reason: a script that reached `exec` cannot have produced
    // this code, and a target that emitted records cannot have failed before it ran.
    if run.rc == 96 && run.stdout.trim().is_empty() {
        return Err(Abort::IsolationScriptFailed {
            probe: probe.to_string(),
            rc: run.rc,
            stderr: run.stderr.trim().to_string(),
        });
    }
    if run.rc == 124 {
        return Err(Abort::TargetTimedOut {
            probe: probe.to_string(),
            secs: target.timeout_secs,
            stderr: run.stderr.trim().to_string(),
        });
    }
    if run.stdout.trim().is_empty() && run.stderr.starts_with("unshare:") {
        return Err(Abort::IsolationUnavailable {
            rc: Some(run.rc),
            stderr: run.stderr.trim().to_string(),
        });
    }

    let mut obs = Observed {
        rc: run.rc,
        ..Default::default()
    };
    let mut started = false;
    let mut terminal = false;
    for line in run.stdout.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            // Fail-closed, and the tripwire if a future libtest changes the format.
            return Err(Abort::HarnessOutputUnparseable {
                probe: probe.to_string(),
                line: line.to_string(),
            });
        };
        match (v["type"].as_str(), v["event"].as_str()) {
            (Some("suite"), Some("started")) => {
                started = true;
                obs.test_count = v["test_count"].as_u64().unwrap_or(0) as usize;
            }
            (Some("suite"), Some(event)) => {
                terminal = true;
                obs.target_failed = event != "ok";
                obs.filtered_out = v["filtered_out"].as_u64().unwrap_or(0) as usize;
                let field = |k| v[k].as_u64().unwrap_or(0) as usize;
                obs.passed = field("passed");
                obs.failed = field("failed");
                obs.ignored = field("ignored");
            }
            // `event:"failed"` appears on the suite record too, so the filter is written on
            // `type`, not on `event`.
            (Some("test"), Some("failed")) => obs
                .failed_captures
                .push(v["stdout"].as_str().unwrap_or_default().to_string()),
            _ => {}
        }
    }
    for (present, missing) in [
        (started, HarnessMarker::SuiteStarted),
        (terminal, HarnessMarker::SuiteFinished),
    ] {
        if !present {
            return Err(Abort::HarnessIncomplete {
                probe: probe.to_string(),
                missing,
                rc: run.rc,
            });
        }
    }
    // The EXECUTION floor, and the general guarantee of the whole tool: not "were tests
    // selected" but "did a test body run". `test_count` answers the first question, and the
    // three shapes that made this crate render a green row for an unmeasured tree — a filter
    // that matched nothing, an `#[ignore]` on the pinned test, a `--bench` argv — are
    // indistinguishable in it (measured: `test_count: 1` for all three). `passed + failed` is
    // the field that answers the question, and it closes every suppressing flag at once,
    // including the ones no denylist names.
    if obs.passed + obs.failed == 0 {
        return Err(Abort::NothingExecuted {
            probe: probe.to_string(),
            reason: if obs.test_count == 0 {
                NothingRan::NoneSelected
            } else {
                NothingRan::AllSkipped
            },
            filter: target.filter.clone(),
            filter_match: target.filter_match,
            test_count: obs.test_count,
            ignored: obs.ignored,
            filtered_out: obs.filtered_out,
            rc: run.rc,
        });
    }
    Ok(obs)
}

/// Every anchor in the list, inside ONE record's capture. Not a whole run's output, and not
/// an inferred prose block.
pub fn all_anchors_in_one(anchors: &[String], captures: &[String]) -> bool {
    captures
        .iter()
        .any(|c| anchors.iter().all(|a| c.contains(a.as_str())))
}

/// A measured outcome that contradicts the manifest's expectation. It carries the measured
/// facts and formats at the boundary, like every other refusal in this crate — `main` is the
/// only thing that turns one into text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub probe: String,
    pub expected: Expect,
    pub observed_rc: i32,
    /// `Some` iff the target failed as expected but an anchor did not land.
    pub missed_anchor: Option<String>,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let probe = &self.probe;
        match (&self.expected, &self.missed_anchor) {
            (_, Some(missed)) => write!(
                f,
                "{probe} failed as expected, but anchor {missed:?} did not hit inside a single failed-test record"
            ),
            (Expect::Pass {}, None) => write!(
                f,
                "{probe} expected outcome = \"pass\" but the target FAILED (exit {})",
                self.observed_rc
            ),
            (Expect::Fail { .. }, None) => write!(
                f,
                "{probe} expected outcome = \"fail\" but the target PASSED (exit {})",
                self.observed_rc
            ),
        }
    }
}

/// `Verdict::Fail` IS "the target failed AND every anchor hit inside one failed-test record".
/// The complement of that — a target failure whose anchors do not all land in one record — is
/// a verdict MISMATCH naming the missed anchor, never a `Pass`, because `Pass`'s rendering
/// would transcribe "visible-and-still-passing" for a run whose target failed.
pub fn verdict(probe: &Probe, obs: &Observed, mounts_reached: usize) -> Result<Verdict, Mismatch> {
    let mismatch = |missed_anchor: Option<String>| Mismatch {
        probe: probe.id.clone(),
        expected: probe.expect.clone(),
        observed_rc: obs.rc,
        missed_anchor,
    };
    match &probe.expect {
        Expect::Pass {} => {
            if obs.target_failed {
                Err(mismatch(None))
            } else {
                Ok(Verdict::Pass { mounts_reached })
            }
        }
        Expect::Fail { anchor } => {
            if !obs.target_failed {
                return Err(mismatch(None));
            }
            if !all_anchors_in_one(anchor, &obs.failed_captures) {
                let missed = anchor
                    .iter()
                    .find(|a| !obs.failed_captures.iter().any(|c| c.contains(a.as_str())))
                    .cloned()
                    .unwrap_or_else(|| anchor.join(" / "));
                return Err(mismatch(Some(missed)));
            }
            Ok(Verdict::Fail {
                provenance: provenance(&obs.failed_captures),
            })
        }
    }
}

/// Sorted, de-duplicated `panicked at <path>` paths. No `:line:col`: a line number is not
/// evidence a committed artifact can carry.
pub fn provenance(captures: &[String]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for c in captures {
        for (_, rest) in c
            .match_indices("panicked at ")
            .map(|(i, m)| (i, &c[i + m.len()..]))
        {
            let path = rest.split([':', '\n']).next().unwrap_or_default().trim();
            if !path.is_empty() {
                let p = PathBuf::from(path);
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// §7 step 9. An anchor list that fully matches ANOTHER probe's captured failure does not
/// distinguish the two probes: the pin would stay green if this probe's mutation stopped
/// firing. The self-hit is the reach guard, and it is enforced by `verdict()` above.
pub fn discriminate(
    probes: &[Probe],
    captures: &BTreeMap<String, Vec<String>>,
) -> Result<(), Abort> {
    for probe in probes {
        let anchors = probe.anchors();
        if anchors.is_empty() {
            continue;
        }
        for (other, other_captures) in captures {
            if *other == probe.id {
                continue;
            }
            if all_anchors_in_one(anchors, other_captures) {
                return Err(Abort::AnchorNotDiscriminating {
                    probe: probe.id.clone(),
                    collides_with: other.clone(),
                });
            }
        }
    }
    Ok(())
}
