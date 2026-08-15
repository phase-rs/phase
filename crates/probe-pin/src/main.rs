//! probe-pin's CLI and the §7 pipeline.
//!
//! Exit contract: **0** ok · **1** verdict mismatch / drift · **2** every `Abort` and every
//! internal error · **101** probe-pin itself panicked (out of contract, documented).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};

use probe_pin::block::{self, Outcome, ProjectionCount};
use probe_pin::manifest::Manifest;
use probe_pin::{isolate, manifest, mutate, project, target, verdict, Abort};

#[derive(Parser)]
#[command(
    name = "probe-pin",
    about = "Pin probe-derived claims to a regenerable block"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run every probe; print the block, or splice it into `[output].file` with `--write`.
    Run {
        #[arg(long)]
        write: bool,
        manifest: PathBuf,
    },
    /// Run every probe and compare against the committed block. Same cost as `run`.
    Check { manifest: PathBuf },
}

/// What step 12 does with the rendered block. Three states, and `Cmd` is the only thing that
/// decides which — a `(write, compare)` bool pair spells one of them twice and one not at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Print,
    Write,
    Compare,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (path, mode) = match &cli.cmd {
        Cmd::Run {
            write: true,
            manifest,
        } => (manifest, Mode::Write),
        Cmd::Run { manifest, .. } => (manifest, Mode::Print),
        Cmd::Check { manifest } => (manifest, Mode::Compare),
    };
    match pipeline(path, mode) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let chain = format!("{e:#}");
            if chain.starts_with("probe-pin:") {
                eprintln!("{chain}");
            } else {
                eprintln!("probe-pin: {chain}");
            }
            ExitCode::from(2)
        }
    }
}

/// One probe's measurement, recorded through `verdict::verdict` — the crate's single verdict
/// authority. Every probe reaches the block through this one function, control included: the
/// probe whose whole job is "the instrument works" is the last one that should be exempt from
/// the check that a measurement matched its declared expectation.
fn record(
    probe: &manifest::Probe,
    obs: &verdict::Observed,
    mounts_reached: usize,
    outcomes: &mut Vec<Outcome>,
    captures: &mut BTreeMap<String, Vec<String>>,
    mismatches: &mut Vec<verdict::Mismatch>,
) {
    captures.insert(probe.id.clone(), obs.failed_captures.clone());
    match verdict::verdict(probe, obs, mounts_reached) {
        Ok(verdict) => outcomes.push(Outcome {
            id: probe.id.clone(),
            rc: obs.rc,
            verdict,
        }),
        Err(mismatch) => mismatches.push(mismatch),
    }
}

fn pipeline(manifest_path: &Path, mode: Mode) -> anyhow::Result<u8> {
    // 1. load + validate
    let m = Manifest::load(manifest_path)?;
    manifest::validate(&m)?;

    // 2. workspace root
    let root = target::workspace_root()?;
    // Workspace-relative or nothing: this string reaches the digest AND the BEGIN line, so an
    // absolute path stamps a machine-specific value into a committed artifact — the exact
    // reproducibility the digest exists to provide (docs/probe-pin.md: "Excluded: … absolute
    // paths"), and the treatment `is_workspace_relative` already gives every other path key.
    // The comparison itself lives in `manifest`, realpath against realpath, beside the other
    // containment assertions rather than inline here where nothing can measure it.
    let manifest_rel = manifest::workspace_relative(manifest_path, &root)?;

    // 2b. the refusals that need something step 1 does not have — the workspace root for path
    // containment, the workspace-relative manifest path for the block's own text. Between
    // resolving the root and building the target, so both are asserted at zero target runs.
    manifest::validate_paths(&m, &root)?;
    manifest::validate_block_text(&m, &manifest_rel)?;

    // 3. resolve the target binary — outside the namespace, on the pristine tree
    let testbin = target::resolve(&root, &m.target.package, &m.target.test)?;

    // 4. record instruments
    let mut instruments = Vec::new();
    for instrument in block::used_instruments(&m) {
        let program = project::program_for(instrument);
        instruments.push((
            instrument,
            project::instrument_version(instrument, &program)?,
        ));
    }

    // 5. scratch, outside the workspace
    let scratch = isolate::scratch_dir(&std::env::temp_dir(), &root)?;

    // 6. fingerprint every mutated file
    let mut touched: Vec<String> = Vec::new();
    for p in &m.probes {
        for f in p.touched() {
            if !touched.contains(&f) {
                touched.push(f);
            }
        }
    }
    let before = isolate::fingerprint(&root, &touched)?;

    // 7. the control, first — its execution floor gates everything downstream
    let control = m.control();
    let run = isolate::run(&testbin, &[], &m.target)?;
    let obs = verdict::observe(&control.id, &run, &m.target, &[])?;
    if obs.target_failed {
        return Err(Abort::ControlFailed {
            probe: control.id.clone(),
            rc: obs.rc,
        }
        .into());
    }
    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut captures: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut mismatches: Vec<verdict::Mismatch> = Vec::new();
    // The control's verdict comes from the same authority as every other probe's. Constructing
    // one here instead would report a control whose expectation the run REFUTES as a clean pass.
    record(
        control,
        &obs,
        0,
        &mut outcomes,
        &mut captures,
        &mut mismatches,
    );

    // 8. every other probe
    for probe in m.probes.iter().filter(|p| !p.mutations.is_empty()) {
        let mounts = mutate::apply(probe, &root, scratch.path())?;
        let run = isolate::run(&testbin, &mounts, &m.target)?;
        let obs = verdict::observe(&probe.id, &run, &m.target, &mounts)?;
        record(
            probe,
            &obs,
            mounts.len(),
            &mut outcomes,
            &mut captures,
            &mut mismatches,
        );
    }

    // 9. all-pairs discrimination over the captured failed-test records
    verdict::discriminate(&m.probes, &captures)?;

    // 10. re-fingerprint — probe-pin must never write the files it mutates
    isolate::verify_fingerprint(&before, &isolate::fingerprint(&root, &touched)?)?;

    // A run with a verdict mismatch exits non-zero and never splices a block.
    if !mismatches.is_empty() {
        for mismatch in &mismatches {
            eprintln!("probe-pin: VERDICT MISMATCH: {mismatch}");
        }
        return Ok(1);
    }

    // 11. projections
    let mut counts = Vec::new();
    for projection in &m.projections {
        counts.push(ProjectionCount {
            id: projection.id.clone(),
            count: project::count(projection, &root, &project::astgrep_tool())?,
        });
    }

    // 12. render, then write or compare
    let digest = block::digest(&m, &manifest_rel, &outcomes, &counts, &instruments)?;
    let generated = block::render(&m, &manifest_rel, &digest, &instruments, &outcomes, &counts);

    if mode == Mode::Print {
        println!("{generated}");
        return Ok(0);
    }

    let out_path = root.join(&m.output.file);
    let text = std::fs::read_to_string(&out_path)
        .with_context(|| format!("probe-pin: cannot read {}", out_path.display()))?;
    let committed = block::extract(&text, &m.output.marker, &out_path)?;
    let outcome = block::check(&committed, &generated);

    if mode == Mode::Compare {
        let report = block::report(&outcome, &manifest_rel);
        if !report.is_empty() {
            eprintln!("{report}");
        }
        return Ok(block::exit_code(&outcome));
    }

    if let Some(abort) = block::write_refusal(&outcome, &committed, &generated) {
        return Err(abort.into());
    }
    std::fs::write(
        &out_path,
        block::splice(&text, &m.output.marker, &out_path, &generated)?,
    )
    .with_context(|| format!("probe-pin: cannot write {}", out_path.display()))?;
    Ok(0)
}
