//! §7 step 12: the digest, the rendered block, the splice, and `check`'s drift classification.

use std::path::{Path, PathBuf};

use serde::Serialize;
use similar::TextDiff;

use crate::manifest::{Expect, Manifest, Mutation, Probe};
use crate::verdict::Verdict;
use crate::{sha256_hex, Abort, CheckOutcome, DriftCause, Instrument, InstrumentChange};

/// What a probe's run produced. Rendering and the digest both read the manifest for the
/// probe's inputs, so this carries only what was MEASURED.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub id: String,
    pub rc: i32,
    pub verdict: Verdict,
}

/// A projection's measured count, paired with the declaration it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionCount {
    pub id: String,
    pub count: usize,
}

// ---------------------------------------------------------------- digest

#[derive(Serialize)]
struct MutationInput<'a> {
    kind: &'static str,
    files: Vec<&'a str>,
    find: Option<String>,
    replace: Option<String>,
    text: Option<String>,
    repeat: Option<u32>,
}

#[derive(Serialize)]
struct ProbeInput<'a> {
    id: &'a str,
    claim: &'a str,
    mutations: Vec<MutationInput<'a>>,
    assert_counts: Vec<(&'a str, &'a str, usize)>,
    expect: &'a Expect,
    rc: i32,
    verdict: &'static str,
    mounts_reached: Option<usize>,
    provenance: Option<Vec<PathBuf>>,
}

#[derive(Serialize)]
struct ProjectionInput<'a> {
    id: &'a str,
    pattern: &'a str,
    paths: Vec<&'a str>,
    count: usize,
}

#[derive(Serialize)]
struct DigestInput<'a> {
    manifest: &'a str,
    target: &'a crate::manifest::Target,
    output: &'a crate::manifest::Output,
    probes: Vec<ProbeInput<'a>>,
    control: &'a str,
    projections: Vec<ProjectionInput<'a>>,
    instruments: &'a [(Instrument, String)],
}

fn mutation_input(m: &Mutation) -> MutationInput<'_> {
    match m {
        Mutation::Replace {
            file,
            find,
            replace,
        } => MutationInput {
            kind: "replace",
            files: vec![file.as_str()],
            find: Some(sha256_hex(find.as_bytes())),
            replace: Some(sha256_hex(replace.as_bytes())),
            text: None,
            repeat: None,
        },
        Mutation::Prepend {
            files,
            text,
            repeat,
        } => MutationInput {
            kind: "prepend",
            files: files.iter().map(String::as_str).collect(),
            find: None,
            replace: None,
            text: Some(sha256_hex(text.as_bytes())),
            repeat: Some(*repeat),
        },
    }
}

/// `sha256(serde_json::to_vec(&DigestInput))`, truncated to 16 hex.
///
/// There is no hand-rolled joiner to review: JSON escaping IS the injection safety (a NUL in
/// an id serializes as `\u0000`) and declaration order IS the canonical order. The digest pins
/// what was MEASURED — the whole `[target]`, `[output]`, and every projection's pattern and
/// sorted paths — not just the outcomes, so narrowing a filter or subsetting a projection's
/// paths flips it even when every verdict and count is identical. Excluded: `:line:col`,
/// durations, PIDs, absolute paths, completion order.
pub fn digest(
    manifest: &Manifest,
    manifest_rel: &str,
    outcomes: &[Outcome],
    counts: &[ProjectionCount],
    instruments: &[(Instrument, String)],
) -> anyhow::Result<String> {
    let probes = manifest
        .probes
        .iter()
        .map(|p| {
            let outcome = outcomes.iter().find(|o| o.id == p.id);
            let verdict = outcome.map(|o| &o.verdict);
            ProbeInput {
                id: &p.id,
                claim: &p.claim,
                mutations: p.mutations.iter().map(mutation_input).collect(),
                assert_counts: p
                    .assert_counts
                    .iter()
                    .map(|a| (a.file.as_str(), a.text.as_str(), a.count))
                    .collect(),
                expect: &p.expect,
                rc: outcome.map(|o| o.rc).unwrap_or(-1),
                verdict: match verdict {
                    Some(Verdict::Pass { .. }) => "pass",
                    Some(Verdict::Fail { .. }) => "fail",
                    None => "unmeasured",
                },
                // Exhaustive, never `_`: these two fields are what the digest PINS about a
                // measurement, and a wildcard would silently record `None` — an unmeasured
                // probe's own value — for a future verdict shape nobody revisited here.
                mounts_reached: match verdict {
                    Some(Verdict::Pass { mounts_reached }) => Some(*mounts_reached),
                    Some(Verdict::Fail { .. }) | None => None,
                },
                provenance: match verdict {
                    Some(Verdict::Fail { provenance }) => Some(provenance.clone()),
                    Some(Verdict::Pass { .. }) | None => None,
                },
            }
        })
        .collect();
    let projections = manifest
        .projections
        .iter()
        .map(|p| {
            let mut paths: Vec<&str> = p.paths.iter().map(String::as_str).collect();
            paths.sort_unstable();
            ProjectionInput {
                id: &p.id,
                pattern: &p.pattern,
                paths,
                count: counts.iter().find(|c| c.id == p.id).map_or(0, |c| c.count),
            }
        })
        .collect();
    let input = DigestInput {
        manifest: manifest_rel,
        target: &manifest.target,
        output: &manifest.output,
        probes,
        control: &manifest.control().id,
        projections,
        instruments,
    };
    let bytes = serde_json::to_vec(&input)?;
    Ok(sha256_hex(&bytes)[..16].to_string())
}

// ---------------------------------------------------------------- render

fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// `visibility.rs ×1` / `engine.rs ×2 (seq)` / `(none)` for the control.
pub fn mutation_cell(probe: &Probe) -> String {
    if probe.mutations.is_empty() {
        return "(none)".to_string();
    }
    let mut sequential = false;
    let parts: Vec<String> = probe
        .touched()
        .iter()
        .map(|rel| {
            let n = probe
                .mutations
                .iter()
                .filter(|m| m.files().iter().any(|f| f == rel))
                .count();
            sequential |= n > 1;
            let base = Path::new(rel)
                .file_name()
                .map_or(rel.as_str(), |s| s.to_str().unwrap_or(rel.as_str()));
            format!("{base} ×{n}")
        })
        .collect();
    format!(
        "{}{}",
        parts.join(", "),
        if sequential { " (seq)" } else { "" }
    )
}

/// Keyed on probe IDENTITY, never on the measured count.
///
/// `mutation_cell` decides "this is the control" from `probe.mutations.is_empty()`, which is the
/// definition — `validate` requires exactly one probe with no mutations. Deciding the same fact
/// here from `mounts_reached == 0` made two discriminators for one concept in adjacent functions,
/// agreeing only through a property of the current call graph: `main` passes a literal `0` for the
/// control and `mounts.len()` for every other probe, and `validate` refuses an empty mutation file
/// list, so a mutation probe cannot reach `0` today.
///
/// That is a coincidence to rest an evidence artifact on, not a guarantee. Any future path
/// yielding a probe with mutations but no materialized mount would render `(control; no mounts)`
/// for a MUTATION probe — a false claim in a committed block, and one no digest catches, because
/// the digest pins `mounts_reached` and would agree with it. A row says what a probe IS; the count
/// says what the run reached. Rendered output is unchanged (measured: the dogfood block and its
/// digest are byte-identical across this change).
fn firing_cell(probe: &Probe, verdict: &Verdict) -> String {
    match verdict {
        Verdict::Pass { .. } if probe.mutations.is_empty() => "(control; no mounts)".to_string(),
        Verdict::Pass { mounts_reached } => format!("mount reached: {mounts_reached} file(s)"),
        Verdict::Fail { .. } => probe.anchors().join(" / "),
    }
}

fn provenance_cell(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Pass { .. } => "—".to_string(),
        Verdict::Fail { provenance } if provenance.is_empty() => "—".to_string(),
        Verdict::Fail { provenance } => provenance
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// Byte-deterministic by construction: probes are iterated through the manifest's declaration
/// `Vec` (never a `HashMap`), and `[target].env` is a `BTreeMap`, so two parses of the same
/// manifest render identical bytes.
pub fn render(
    manifest: &Manifest,
    manifest_rel: &str,
    digest: &str,
    instruments: &[(Instrument, String)],
    outcomes: &[Outcome],
    counts: &[ProjectionCount],
) -> String {
    let marker = &manifest.output.marker;
    let mut out = String::new();
    out.push_str(&format!(
        "// {marker}:BEGIN manifest={manifest_rel} digest=sha256:{digest}\n"
    ));
    for (instrument, version) in instruments {
        out.push_str(&format!("// instrument {instrument} = {version}\n"));
    }
    out.push_str(
        "// | probe | mutation | expect | verdict | firing assertion (anchor) | provenance |\n",
    );
    out.push_str("// |---|---|---|---|---|---|\n");
    for probe in &manifest.probes {
        let Some(outcome) = outcomes.iter().find(|o| o.id == probe.id) else {
            // An outcome-less probe gets a VISIBLE row saying so, matching what `digest` already
            // records for the same input (`rc = -1`, `verdict = "unmeasured"`).
            //
            // This used to `continue`, which made the two consumers of one probe list disagree
            // about what a missing outcome MEANS — in the direction this crate exists to refuse.
            // A manifest declares N probes, the block shows N-1 rows, and `check` reports CLEAN,
            // because the committed and the generated block omit the same row and stamp the same
            // digest. Evidence disappears and nothing is loud.
            //
            // Not reachable through `main` today: `record` routes an expectation mismatch to
            // `mismatches` rather than `outcomes`, and `pipeline` returns at that gate
            // (`main.rs:190`) before `render` runs. An `unreachable!` was tried here and was
            // wrong — `render` is public and tests legitimately call it with `&[]` outcomes to
            // inspect projection sentences, so the panic fired on correct use. A function whose
            // contract only holds for one caller's call order should not be enforcing that order;
            // it should represent the state it can actually be handed.
            out.push_str(&format!(
                "// | {} | {} | {} | unmeasured | (not measured) | — |\n",
                cell(&probe.id),
                cell(&mutation_cell(probe)),
                match probe.expect {
                    Expect::Pass {} => "pass",
                    Expect::Fail { .. } => "fail",
                },
            ));
            continue;
        };
        let expect = match probe.expect {
            Expect::Pass {} => "pass",
            Expect::Fail { .. } => "fail",
        };
        let verdict = match outcome.verdict {
            Verdict::Pass { .. } => "pass",
            Verdict::Fail { .. } => "fail",
        };
        out.push_str(&format!(
            "// | {} | {} | {expect} | {verdict} | {} | {} |\n",
            cell(&probe.id),
            cell(&mutation_cell(probe)),
            cell(&firing_cell(probe, &outcome.verdict)),
            cell(&provenance_cell(&outcome.verdict)),
        ));
    }
    for projection in &manifest.projections {
        let count = counts
            .iter()
            .find(|c| c.id == projection.id)
            .map_or(0, |c| c.count);
        out.push_str(&format!(
            "// {}\n",
            crate::project::sentence(projection, count)
        ));
    }
    out.push_str("// probe-pin validates only the lines between BEGIN and END. Prose outside is never checked.\n");
    out.push_str(&format!("// {marker}:END"));
    out
}

/// Which instruments this run may record, in `Instrument` declaration order.
pub fn used_instruments(manifest: &Manifest) -> Vec<Instrument> {
    let mut v = vec![Instrument::Toolchain];
    if !manifest.projections.is_empty() {
        v.push(Instrument::AstGrep);
    }
    v
}

// ---------------------------------------------------------------- splice

/// Byte offsets of the BEGIN line's start and the END line's last content byte. Everything
/// outside that span — prose above, prose below, and the END line's own terminator — is the
/// caller's to copy verbatim.
pub fn locate(text: &str, marker: &str, file: &Path) -> Result<(usize, usize), Abort> {
    let begin_tag = format!("{marker}:BEGIN");
    let end_tag = format!("{marker}:END");
    let (mut begins, mut ends) = (0usize, 0usize);
    let (mut begin_at, mut end_at) = (None, None);
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches('\n').trim_end_matches('\r');
        if content.contains(&begin_tag) {
            begins += 1;
            begin_at.get_or_insert(offset);
        }
        if content.contains(&end_tag) {
            ends += 1;
            end_at.get_or_insert(offset + content.len());
        }
        offset += line.len();
    }
    match (begin_at, end_at) {
        (Some(b), Some(e)) if begins == 1 && ends == 1 && b < e => Ok((b, e)),
        _ => Err(Abort::MarkerNotUnique {
            file: file.to_path_buf(),
            begins,
            ends,
        }),
    }
}

/// Splices `block` over the marked span, and refuses to return a result the next `locate` could
/// not find.
///
/// The postcondition is checked HERE, at the write surface, and not only on the manifest fields
/// that feed `render`. Validation of those fields is necessary and is done — but it can only
/// cover values probe-pin can see before the run. Two producers reach a rendered line and cannot
/// be refused in advance: an instrument's `--version` output, and the `provenance` paths lifted
/// from the target's own panic text. If any producer emits a marker tag, `run --write` would
/// otherwise commit a block that its own next `check` aborts on with `MarkerNotUnique` — a pin
/// that can only be recovered by editing the file by hand, which is the regenerable-artifact
/// guarantee failing exactly where it is supposed to hold.
///
/// So the invariant is stated against the bytes actually being written, where it is unconditional:
/// whatever produced this text, the file probe-pin is about to commit is one `check` can locate.
pub fn splice(text: &str, marker: &str, file: &Path, block: &str) -> Result<String, Abort> {
    let (begin, end) = locate(text, marker, file)?;
    let spliced = format!("{}{block}{}", &text[..begin], &text[end..]);
    locate(&spliced, marker, file)?;
    Ok(spliced)
}

pub fn extract(text: &str, marker: &str, file: &Path) -> Result<String, Abort> {
    let (begin, end) = locate(text, marker, file)?;
    Ok(text[begin..end].to_string())
}

// ---------------------------------------------------------------- check

/// A block's two regions, split by CONSTRUCTION order rather than by what a line says:
/// `render` emits the BEGIN line, then exactly one `// instrument <tool> = <version>` line per
/// recorded instrument, then everything that carries a measured number. Every line after that
/// contiguous leading run is measured — including a projection sentence that happens to begin
/// with the word `instrument`, which a whole-block prefix test read as an instrument line and
/// silently dropped from the "a measured number moved" comparison, disabling the write refusal.
/// One split, both consumers: the two classifications cannot disagree about a line.
fn regions(block: &str) -> (Vec<&str>, Vec<&str>) {
    let (mut header, mut measured) = (Vec::new(), Vec::new());
    for line in block.lines().skip(1) {
        if measured.is_empty() && line.starts_with("// instrument ") {
            header.push(line);
        } else {
            measured.push(line);
        }
    }
    (header, measured)
}

fn instrument_lines(block: &str) -> Vec<(Instrument, String)> {
    regions(block)
        .0
        .into_iter()
        .filter_map(|l| l.trim_end_matches('\r').strip_prefix("// instrument "))
        .filter_map(|rest| rest.split_once(" = "))
        .filter_map(|(name, version)| {
            [Instrument::Toolchain, Instrument::AstGrep]
                .into_iter()
                .find(|i| i.tool() == name)
                .map(|i| (i, version.to_string()))
        })
        .collect()
}

/// Everything the block asserts about the world MINUS the BEGIN line and the lines that name
/// the tools: the table rows and the projection sentences. If these differ, a measured number
/// moved.
fn measured_lines(block: &str) -> Vec<&str> {
    regions(block).1
}

fn changed_instruments(committed: &str, generated: &str) -> Vec<InstrumentChange> {
    let old = instrument_lines(committed);
    let new = instrument_lines(generated);
    let mut out = Vec::new();
    for (instrument, now) in &new {
        // An instrument the committed block never recorded has not MOVED — there is no prior
        // reading to conflict with. This is what makes the write refusal's documented escape
        // work: reduce the block to a bare BEGIN/END pair and re-run.
        let Some((_, was)) = old.iter().find(|(i, _)| i == instrument) else {
            continue;
        };
        if was != now {
            let was = was.clone();
            out.push(InstrumentChange {
                instrument: *instrument,
                was,
                now: now.clone(),
            });
        }
    }
    out
}

/// The measured-number deltas, for the write refusal's message.
pub fn deltas(committed: &str, generated: &str) -> String {
    let (old, new) = (measured_lines(committed), measured_lines(generated));
    let mut out = Vec::new();
    for change in TextDiff::from_slices(&old, &new).iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Delete => out.push(format!("- {}", change.value().trim())),
            similar::ChangeTag::Insert => out.push(format!("+ {}", change.value().trim())),
            similar::ChangeTag::Equal => {}
        }
    }
    out.join(" ")
}

pub fn check(committed: &str, generated: &str) -> CheckOutcome {
    if committed == generated {
        return CheckOutcome::Clean;
    }
    let changed = changed_instruments(committed, generated);
    let numbers_moved = measured_lines(committed) != measured_lines(generated);
    CheckOutcome::Drift {
        cause: if changed.is_empty() {
            DriftCause::Code
        } else {
            DriftCause::Instrument(changed)
        },
        diff: TextDiff::from_lines(committed, generated)
            .unified_diff()
            .header("committed", "generated")
            .to_string(),
        numbers_moved,
    }
}

/// `run --write` refuses IFF an instrument moved AND a measured number moved — exactly when
/// attribution is impossible AND obeying would destroy the evidence of which one changed it.
/// Escaping needs no new flag: delete the block and re-run, which shows up in the PR diff.
pub fn write_refusal(outcome: &CheckOutcome, committed: &str, generated: &str) -> Option<Abort> {
    match outcome {
        CheckOutcome::Drift {
            cause: DriftCause::Instrument(changed),
            numbers_moved: true,
            ..
        } => Some(Abort::WriteUnderInstrumentChange {
            changed: changed.clone(),
            deltas: deltas(committed, generated),
        }),
        _ => None,
    }
}

/// 0 clean, 1 drift. Never 2: an abort is an instrument failure and never reaches `check`.
pub fn exit_code(outcome: &CheckOutcome) -> u8 {
    match outcome {
        CheckOutcome::Clean => 0,
        CheckOutcome::Drift { .. } => 1,
    }
}

pub fn report(outcome: &CheckOutcome, manifest_rel: &str) -> String {
    match outcome {
        CheckOutcome::Clean => String::new(),
        CheckOutcome::Drift {
            cause,
            diff,
            numbers_moved,
        } => {
            let head = match cause {
                DriftCause::Code => "probe-pin: the generated block is stale.".to_string(),
                DriftCause::Instrument(changed) => format!(
                    "probe-pin: the generated block differs AND {} instrument(s) moved: {}. {}",
                    changed.len(),
                    crate::describe_changes(changed),
                    if *numbers_moved {
                        "A measured number moved too, so which one changed it cannot be attributed."
                    } else {
                        "Measured numbers are unchanged, so this is an instrument change, not code drift."
                    }
                ),
            };
            format!("{diff}\n{head} Re-run: cargo probe-pin run --write {manifest_rel}")
        }
    }
}
