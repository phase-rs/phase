// pod-lab loop-3 Q5: native-binary throughput lever, gated in Cargo.toml so
// wasm32 builds of this crate's lib (pulled in by engine-wasm/draft-wasm)
// never see it.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use engine::database::CardDatabase;
use phase_ai::config::AiDifficulty;
use phase_ai::duel_suite::compare::{
    compare, emit_gate_verdict, load_report, print_markdown, render_error_markdown, CompareError,
    CompareOptions,
};
use phase_ai::duel_suite::run::{run_suite, ReportSink, SuiteOptions};

const DEFAULT_BASELINE: &str = "crates/phase-ai/baselines/suite-baseline.json";
const DEFAULT_CURRENT: &str = "target/ai-gate-current.json";
// Quick PR-gate matchup set (comma-separated id substrings). `red-mirror` is the
// fast aggro-mirror smoke; `affinity-mirror` and `enchantress-mirror` are the
// floor-crossing artifacts/enchantments decks that exercise ArtifactSynergyPolicy
// and EnchantmentsPayoffPolicy (commitment >= COMMITMENT_FLOOR), so the required
// gate actually runs the policies these baselines are meant to guard.
const DEFAULT_QUICK_FILTER: &str = "red-mirror,affinity-mirror,enchantress-mirror";
const DEFAULT_SEED: u64 = 0xA1_57A1;

struct Args {
    data_root: PathBuf,
    baseline: PathBuf,
    current_output: PathBuf,
    games: usize,
    seed: u64,
    difficulty: AiDifficulty,
    suite_filter: Option<String>,
    refresh_baseline: bool,
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            print_usage();
            std::process::exit(2);
        }
    };

    // Refuse before ANY work — before the card database, before a single game — when the
    // suite's output path and the baseline are the same file. Review found this defeats the
    // refusal this PR adds: `run_suite` writes its report to `options.output` before any
    // guard runs, so an aliased pair truncated the baseline and only THEN printed "refusing to
    // refresh". Measured on the real binary at the reviewed head: 116 bytes in, 250 bytes out,
    // different sha256, exit 1.
    //
    // The compare path is worse and is why this check is not confined to `--refresh-baseline`.
    // There the same aliasing makes `load_report(&args.baseline)` read back the run that just
    // overwrote it, so the gate compares the run to ITSELF and exits 0. Measured: a baseline
    // recording p0 at 100% against a run that scored 0% printed `0% | 0%`, zero flips, `0 FAIL`,
    // exit 0. A gate that reports no drift because it destroyed its own reference is the exact
    // false-green this branch exists to close, and it is silent where the refresh case is loud.
    if same_file(&args.baseline, &args.current_output) {
        eprintln!(
            "--baseline and --current-output are the same file ({}); the suite would overwrite \
             the baseline before it could be compared or validated",
            args.baseline.display()
        );
        std::process::exit(2);
    }

    let db_path = args.data_root.join("card-data.json");
    let db = match CardDatabase::from_export(&db_path) {
        Ok(db) => db,
        Err(err) => {
            eprintln!(
                "failed to load card database from {}: {err}",
                db_path.display()
            );
            std::process::exit(2);
        }
    };

    let mut options = SuiteOptions::new(args.difficulty, args.games, args.seed);
    // `--baseline` names an entry, not necessarily a file. Resolve it to the file it designates
    // BEFORE anything is derived from it, because two things downstream have to agree on that
    // one answer: the staging sibling and the promotion target.
    let destination = args
        .refresh_baseline
        .then(|| resolve_destination(&args.baseline));
    // On a refresh, the suite writes to a staging file BESIDE the baseline rather than to
    // `--current-output`, and the baseline is replaced by renaming that file only after every
    // guard has passed. The alias check above already refuses the one path that reached this
    // bug, but a check on argument values cannot be the whole answer: the property that has to
    // hold is that a rejected run never modifies the baseline, and that is a property of the
    // write ordering, not of the flags. Staging + rename gives it unconditionally.
    //
    // Beside the baseline because `rename` is only atomic within one filesystem; a staging file
    // in `/tmp` could land on a different mount and silently degrade to copy-then-truncate.
    // `--current-output` is therefore unused on the refresh path — the refreshed baseline IS
    // the run's report. No workflow passes `--refresh-baseline`, so nothing in CI depends on
    // the old behaviour.
    let staging = destination.as_deref().map(staging_path);
    // RESERVE the staging path before the suite can write a byte to it.
    //
    // Third route to the same destruction, and the only one no argument check could ever catch:
    // this path is derived internally, so `same_file` never sees it — it compares `--baseline`
    // against `--current-output`, and the staging file is neither. `write_report` opened it by
    // name with `File::create`, which FOLLOWS a symlink to its target and SHARES a hard link's
    // inode, so an entry already sitting there truncated whatever it pointed at, before any
    // refresh guard ran.
    // Measured on the binary before this reservation existed: with a symlink pre-placed at the
    // staging path, the refresh reported success and the baseline's bytes were gone.
    //
    // `create_new` is the whole fix, and it is deliberately a reservation rather than a check.
    // Testing the path first and opening it second leaves the window between them, which is the
    // same class of bug one layer down; `O_CREAT|O_EXCL` fails on ANY existing entry — regular
    // file, hard link, live or dangling symlink — in one atomic step. What the suite then
    // truncates is a regular file this process just created, with a link count of one.
    let mut reserved = None;
    if let Some(path) = &staging {
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                eprintln!("failed to create {}: {err}", parent.display());
                std::process::exit(2);
            }
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            // Retained, not dropped: the suite writes the report through this descriptor, so an
            // entry that replaces `path` after this point can no longer be followed or truncated.
            Ok(file) => reserved = Some(file),
            // Refusing rather than reusing is the point. The path is derived, so anything already
            // there was not put there by this run, and a leftover from a killed run is exactly
            // the artefact the refusal paths below delete. Naming it and stopping is better than
            // writing through something whose provenance is unknown.
            Err(err) => {
                eprintln!(
                    "failed to reserve the staging file {}: {err}\n\
                     if a previous run was interrupted, remove that file and retry; if it is a \
                     symlink or hard link, it would have been written straight through into \
                     whatever it points at",
                    path.display()
                );
                std::process::exit(2);
            }
        }
    }
    options.output = match reserved {
        Some(file) => ReportSink::Reserved(file),
        None => ReportSink::Create(args.current_output.clone()),
    };
    options.filter = args.suite_filter.clone();
    options.git_sha = command_output("git", &["rev-parse", "--short=12", "HEAD"]);
    options.card_data_hash = command_output("git", &["hash-object", path_str(&db_path)]);

    // Read the baseline BEFORE the suite runs, and keep it in memory.
    //
    // This is the root-cause half of the aliasing fix. The check above now recognises hard links
    // too, so it is no longer the case that an alias can slip past it — but the two guards answer
    // different questions and only one of them survives being wrong. `same_file` enumerates the
    // ways two names can mean one file, and any such enumeration is a claim about the filesystem
    // that a future filesystem can falsify. Reading first makes the COMPARISON independent of
    // anything the suite writes, whatever the check missed. It cannot save the bytes — only
    // refusing does that — so this is defence in depth, not a substitute.
    //
    // It also fails a missing or corrupt baseline in a second instead of after a full suite run,
    // which is the difference between a typo costing nothing and costing a hundred games.
    let baseline = match load_report(&args.baseline) {
        Ok(report) => Some(report),
        // ABSENT is the only error a refresh may proceed through, and the narrowness is the
        // point. Review found the earlier `Err(_) if refresh_baseline` arm logged every failure
        // and carried on to `run_suite`, which then renamed the staged report over the file — so
        // a baseline that was corrupt, truncated, or unreadable was DESTROYED rather than kept
        // for diagnosis, and the replacement was established from an unexamined prior state.
        // "I could not read it" is not evidence that it was worthless.
        //
        // Matched on `ErrorKind::NotFound` rather than `!args.baseline.exists()`: the old form
        // asked a second, later question of the filesystem and answered the wrong one under a
        // permission error, where the file exists but `exists()` reports false.
        Err(CompareError::Io(err))
            if args.refresh_baseline && err.kind() == ErrorKind::NotFound =>
        {
            None
        }
        Err(err) => {
            // Every exit after the reservation has to release it, or a refused run leaves a file
            // that blocks the next refresh — turning one diagnosable failure into two.
            release_staging(&staging);
            // Same reasoning as the compare refusal below: the nightly posts stdout, so a
            // read failure that spoke only to stderr produced a red job whose issue body was
            // the suite table and no statement of what went wrong. This is also the only
            // caller that can reach `render_error_markdown`'s I/O arm — `compare` does no
            // I/O, so before this the arm existed and was unreachable.
            eprintln!("failed to load baseline {}: {err}", args.baseline.display());
            print!("{}", render_error_markdown(&err));
            std::process::exit(2);
        }
    };

    let current = match run_suite(&db, &options) {
        Ok(report) => report,
        Err(err) => {
            release_staging(&staging);
            eprintln!("suite run failed: {err}");
            std::process::exit(1);
        }
    };

    if args.refresh_baseline {
        // A baseline is what every later run is judged against, so refreshing from a run
        // that failed its own `Expected` check blesses that failure permanently: the next
        // run compares equal to it and exits 0 forever, and the gate goes quiet about a
        // matchup that is still broken. Refuse. A matchup that genuinely has no verdict
        // yet says so with `Expected::Open` in the suite definition — that is the place
        // to express it, not a red baseline.
        //
        // ORDER MATTERS, and this one is strictly better on every input. The two conditions
        // are not exclusive: `failed_result` builds a matchup with an empty `games` vector
        // AND `SuiteStatus::Fail`, so a run whose deck payloads all failed to load satisfies
        // both. Reporting the failures names each matchup and its `setup error: …`; reporting
        // gamelessness first would replace that with a sentence about seeds. Nothing is lost
        // by checking failures first, because a run that is merely gameless — a
        // `--suite-filter` matching nothing — has no failing matchups to report.
        let staging = staging.expect("staging path is set whenever refresh_baseline is");
        let destination =
            destination.expect("the destination is resolved whenever refresh_baseline is");
        // Every exit below leaves the staging file behind otherwise, and a stale
        // `*.staging.json` next to a baseline is exactly the kind of artefact someone later
        // mistakes for a real one.
        let refuse = |message: &str| -> ! {
            let _ = std::fs::remove_file(&staging);
            eprintln!("{message}");
            std::process::exit(1);
        };

        let failing: Vec<_> = current.failing_matchups().collect();
        if !failing.is_empty() {
            eprintln!(
                "refusing to refresh {}: {} matchup(s) failed their own suite check",
                args.baseline.display(),
                failing.len()
            );
            for result in failing {
                eprintln!(
                    "  {}: {}",
                    result.matchup_id,
                    result
                        .fail_reason
                        .as_deref()
                        .unwrap_or("no reason recorded")
                );
            }
            refuse(
                "fix the regression, or declare the matchup `Expected::Open` if it has no verdict yet",
            );
        }
        // A run that measured nothing is unfit for the same reason a red one is, reached from
        // the other side: comparison pairs by seed, so a gameless baseline scores zero on the
        // outcome axes forever and the drift signal dies quietly. Reached by a `--suite-filter`
        // that selects no matchups; `--games 0` is refused earlier, at parse time.
        if current.recorded_games() == 0 {
            refuse(&format!(
                "refusing to refresh {}: the run recorded no games, so every later comparison would score zero",
                args.baseline.display()
            ));
        }
        // Informational old-vs-new diff, from the copy read before the suite ran.
        if let Some(old) = &baseline {
            match compare(old, &current, &CompareOptions) {
                Ok(report) => print_markdown(&report),
                Err(err) => eprintln!("could not compare old baseline: {err}"),
            }
        }
        // The run is accepted: promote the staging file. `rename` replaces the baseline in one
        // step, so a reader never observes a half-written baseline and a failure here leaves the
        // previous one intact.
        if let Err(err) = std::fs::rename(&staging, &destination) {
            let _ = std::fs::remove_file(&staging);
            eprintln!("failed to write baseline {}: {err}", destination.display());
            std::process::exit(1);
        }
        eprintln!("baseline refreshed at {}", destination.display());
        return;
    }

    let baseline =
        baseline.expect("the non-refresh path exits above when the baseline is unreadable");

    // stdout carries the report body — the nightly redirects it into the file it posts as a
    // drift issue — so a refusal has to be printed there too, not only to stderr. `gate_verdict`
    // owns both halves so the pair is testable; `main` prints and exits.
    let comparison = compare(&baseline, &current, &CompareOptions);
    if let Err(err) = &comparison {
        eprintln!("compare failed: {err}");
    }
    let code = emit_gate_verdict(&comparison);
    if code != 0 {
        std::process::exit(code);
    }
}

fn parse_args() -> Result<Args, String> {
    let mut data_root = std::env::var("PHASE_CARDS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data"));
    let mut baseline = PathBuf::from(DEFAULT_BASELINE);
    let mut current_output = PathBuf::from(DEFAULT_CURRENT);
    let mut games = 10usize;
    let mut seed = DEFAULT_SEED;
    let mut difficulty = AiDifficulty::Medium;
    let mut suite_filter = Some(DEFAULT_QUICK_FILTER.to_string());
    let mut refresh_baseline = false;

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--data-root" => {
                data_root = next_path(&mut iter, "--data-root")?;
            }
            "--baseline" => {
                baseline = next_path(&mut iter, "--baseline")?;
            }
            "--current-output" => {
                current_output = next_path(&mut iter, "--current-output")?;
            }
            "--games" => {
                // `usize` alone accepts 0, which the error string already promised it would
                // not. A zero-game run classifies every matchup `Open` and produces a
                // baseline that can never detect drift, so reject it here rather than
                // burning a whole suite run to refuse it later.
                games = match next_value(&mut iter, "--games")?.parse() {
                    Ok(0) | Err(_) => return Err("--games must be a positive integer".to_string()),
                    Ok(value) => value,
                };
            }
            "--seed" => {
                seed = next_value(&mut iter, "--seed")?
                    .parse()
                    .map_err(|_| "--seed must be an integer".to_string())?;
            }
            "--difficulty" => {
                // Case-insensitive; unknown labels fall back to Medium via
                // `AiDifficulty::from_label`. Run the same difficulty on branch
                // and baseline so the pair isolates the code delta.
                difficulty = AiDifficulty::from_label(&next_value(&mut iter, "--difficulty")?);
            }
            "--suite-filter" => {
                suite_filter = Some(next_value(&mut iter, "--suite-filter")?);
            }
            "--full-suite" => suite_filter = None,
            "--refresh-baseline" => refresh_baseline = true,
            "--help" | "-h" => return Err(String::new()),
            _ => return Err(format!("unknown option: {arg}")),
        }
    }

    Ok(Args {
        data_root,
        baseline,
        current_output,
        games,
        seed,
        difficulty,
        suite_filter,
        refresh_baseline,
    })
}

fn next_path(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<PathBuf, String> {
    next_value(iter, flag).map(PathBuf::from)
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap_or("")
}

/// Whether two paths designate the same file, including through symlinks, `..`, and hard links.
///
/// Two layers, because they answer different questions and the cheap one is not sufficient.
///
/// **Identity first.** When both paths exist, `(dev, ino)` is the only thing that sees a hard
/// link: `canonicalize` faithfully preserves two distinct names for one inode, so a path
/// comparison calls them different files while `File::create` on either truncates both
/// (`duel_suite::run::write_report`). Review found this and it is not hypothetical — it is the
/// one alias a check on argument strings can never catch, and the destructive one.
///
/// **Paths second.** The current-output side usually does NOT exist yet, so there is no inode to
/// compare; that case falls back to `canonicalize`, which still resolves symlinks and `..`, on
/// the parent directory (which has to be real for the write to land) with the file name rejoined.
///
/// Returns false when neither resolution is possible, which is the right default: an
/// unresolvable path cannot be shown to alias, and refusing to run on a path we cannot inspect
/// would break invocations that are fine.
fn same_file(a: &Path, b: &Path) -> bool {
    // Both sides exist => filesystem identity is decisive, and it subsumes the path check.
    if let Some(identical) = same_inode(a, b) {
        return identical;
    }
    fn resolved(path: &Path) -> Option<PathBuf> {
        if let Ok(canonical) = std::fs::canonicalize(path) {
            return Some(canonical);
        }
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        Some(std::fs::canonicalize(parent).ok()?.join(path.file_name()?))
    }
    match (resolved(a), resolved(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Release a reserved staging file, if this run reserved one.
///
/// A no-op on the compare path, where `staging` is `None`. Errors are ignored deliberately: this
/// only ever runs on a path that is already exiting with a diagnosis of its own, and a failure to
/// remove a scratch file is not worth displacing that diagnosis.
fn release_staging(staging: &Option<PathBuf>) {
    if let Some(path) = staging {
        let _ = std::fs::remove_file(path);
    }
}

/// `Some(true)` when both paths exist and name one inode, `Some(false)` when both exist and do
/// not, `None` when the question cannot be answered — either side missing, or a platform with no
/// inode concept, both of which leave the decision to the path-based fallback.
///
/// Split out rather than inlined so the `cfg` seam is one function with one contract, instead of
/// a conditional block inside a predicate whose meaning would then differ per platform.
#[cfg(unix)]
fn same_inode(a: &Path, b: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let (a, b) = (std::fs::metadata(a).ok()?, std::fs::metadata(b).ok()?);
    Some(a.dev() == b.dev() && a.ino() == b.ino())
}

/// Windows has no inode, but it has the same *question*: `GetFileInformationByHandle` reports a
/// volume serial number and a file index, and that pair is what two hard links to one file share.
/// `same_file::is_same_file` asks exactly that (via `winapi-util`), which the standard library
/// exposes only behind the unstable `windows_by_handle` feature.
///
/// `.ok()` collapses every failure — either path missing, or unopenable — into the same `None`
/// the Unix arm returns for a missing path: unanswerable, decided by the path fallback.
#[cfg(windows)]
fn same_inode(a: &Path, b: &Path) -> Option<bool> {
    same_file::is_same_file(a, b).ok()
}

/// Everything else (wasm32, redox) has no portable file identity, so the question is unanswerable
/// and the path check stands alone. This arm exists so the crate still builds there.
#[cfg(not(any(unix, windows)))]
fn same_inode(_a: &Path, _b: &Path) -> Option<bool> {
    None
}

/// The file `--baseline` designates, following symlinks the way the write it replaces did.
///
/// A refresh used to open the baseline with `File::create`, which follows a symlink chain and
/// refreshes the file at its end. Staging + `rename` does not: `rename` acts on the entry, so
/// promoting onto the supplied spelling would replace the link with a regular file and leave its
/// target stale. Resolving first restores the old destination and is also what keeps the
/// promotion atomic — the staging file is a sibling of THIS path, and `rename` is only atomic
/// within one filesystem, so a link that crosses a mount would otherwise stage on one device and
/// rename onto another (`EXDEV`).
///
/// Not `canonicalize`, on three counts: it requires every component to exist, so a dangling link
/// and a first-ever refresh both fail where `File::create` succeeded; it rewrites a relative path
/// the caller typed into an absolute one, which then appears in every message; and on Windows it
/// returns a `\\?\` UNC path. Reading only the links that are actually there leaves a plain path
/// exactly as supplied.
///
/// Hard links are deliberately NOT followed — a hard link has no target to follow, it IS the
/// file. `rename` breaks the link rather than truncating the shared inode, which is the less
/// destructive of the two.
///
/// The budget bounds a symlink cycle, which would otherwise spin here forever. Exhausting it
/// leaves a still-unresolved path, and nothing acts on it: the kernel refuses the same chain, so
/// `load_report` below fails with `FilesystemLoop` — not `NotFound` — and the run exits before
/// the promotion.
fn resolve_destination(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    // Linux's own `MAXSYMLINKS`; a chain this deep is already refused by every open() on it.
    for _ in 0..40 {
        // `Err` is the ordinary exit: not a symlink, or not there at all.
        let Ok(target) = std::fs::read_link(&current) else {
            return current;
        };
        current = match current.parent() {
            // A link's target is relative to the directory the link sits in, not to the cwd.
            Some(parent) if target.is_relative() => parent.join(target),
            _ => target,
        };
    }
    current
}

/// Where a refresh run stages its report before it earns the right to be the baseline.
///
/// Beside the baseline, so the later `rename` is a same-filesystem atomic replace.
fn staging_path(baseline: &Path) -> PathBuf {
    let name = baseline
        .file_name()
        .map(|n| {
            let mut s = n.to_os_string();
            s.push(".staging.json");
            s
        })
        .unwrap_or_else(|| "baseline.staging.json".into());
    baseline.with_file_name(name)
}

fn print_usage() {
    eprintln!("Usage: cargo ai-gate [--refresh-baseline] [--games N] [--seed S]");
    eprintln!("                     [--difficulty {{medium|hard|veryhard|cedh}}]");
    eprintln!("                     [--suite-filter STR[,STR...] | --full-suite]");
    eprintln!("                     [--data-root DIR] [--baseline PATH] [--current-output PATH]");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sibling property has to hold of the RESOLVED destination, not of the spelling the
    /// caller typed. `rename` is only atomic within one filesystem, so a staging file left beside
    /// a link whose target lives on another mount would fail with `EXDEV`; measured directly,
    /// tmpfs staging renamed onto a btrfs target returns `Invalid cross-device link (os error 18)`.
    /// A cross-filesystem fixture is not constructible in a portable test, so the property is
    /// pinned here, at the derivation.
    #[test]
    fn the_staging_file_is_a_sibling_of_the_resolved_destination() {
        let dir = std::env::temp_dir().join(format!("phase-resolve-dest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).expect("scratch dir");
        let target = dir.join("real/baseline.json");
        std::fs::write(&target, "{}").expect("write");
        #[cfg(unix)]
        {
            let link = dir.join("link.json");
            std::os::unix::fs::symlink(&target, &link).expect("symlink");
            assert_eq!(
                resolve_destination(&link),
                target,
                "the link must resolve to its target"
            );
            assert_eq!(
                staging_path(&resolve_destination(&link)).parent(),
                target.parent(),
                "staging must sit beside the resolved destination, not beside the link"
            );
            // PREMISE: the two spellings really do have different parents, so the assertion above
            // is about resolution and not about a trivially equal comparison.
            assert_ne!(link.parent(), target.parent());
        }
        // Control: a path that is not a link comes back exactly as supplied — a relative spelling
        // stays relative, which is what keeps `baseline refreshed at ...` printing what the caller
        // typed (and, on Windows, keeps a `\\?\` prefix out of it).
        let plain = Path::new(DEFAULT_BASELINE);
        assert_eq!(resolve_destination(plain), plain);
        assert_eq!(resolve_destination(&target), target);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The staging file must be a SIBLING of the baseline. `rename` is only atomic within one
    /// filesystem, so a staging path that drifted to `/tmp` (or anywhere else the baseline is
    /// not) would silently degrade the final replace into copy-then-truncate — reintroducing the
    /// half-written baseline this staging exists to prevent, and doing it invisibly.
    ///
    /// Asserted as "same parent, different file name", which is the property atomicity needs,
    /// rather than as a literal string, which would pin a spelling nobody depends on.
    #[test]
    fn the_staging_file_is_a_sibling_of_the_baseline_it_will_replace() {
        for baseline in [
            "crates/phase-ai/baselines/suite-baseline.json",
            "/abs/path/base.json",
            "relative.json",
            "/weird/no-extension",
        ] {
            let baseline = Path::new(baseline);
            let staging = staging_path(baseline);
            assert_eq!(
                staging.parent(),
                baseline.parent(),
                "staging must sit beside {}, got {}",
                baseline.display(),
                staging.display()
            );
            assert_ne!(
                staging,
                baseline,
                "staging must not BE the baseline: {}",
                baseline.display()
            );
        }
    }

    /// A path and a symlink to it are the same file, and a string comparison cannot see that.
    /// This is the case that makes `same_file` more than `a == b`: the write lands on the
    /// baseline's bytes either way.
    #[test]
    fn same_file_sees_through_a_symlink() {
        let dir = std::env::temp_dir().join(format!("phase-same-file-{}", std::process::id()));
        // A pid is reused, and a failed run leaves its scratch dir behind; without this the next
        // run dies at `symlink()` with EEXIST, which reads as an assertion failure.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let real = dir.join("baseline.json");
        std::fs::write(&real, "{}").expect("write");
        let link = dir.join("link.json");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        #[cfg(unix)]
        {
            assert!(same_file(&real, &link), "symlinked alias must be detected");
            // PREMISE: the two paths really are textually different, so the assertion above is
            // about resolution rather than about a trivially equal comparison.
            assert_ne!(real, link);
        }

        // Control: two genuinely distinct files must not be called aliases, or the guard would
        // refuse every legitimate invocation.
        let other = dir.join("current.json");
        std::fs::write(&other, "{}").expect("write");
        assert!(!same_file(&real, &other));
        // And a path that does not exist yet still resolves through its parent, which is the
        // normal case for `--current-output` on a clean tree.
        assert!(!same_file(&real, &dir.join("not-created-yet.json")));
        assert!(same_file(&real, &dir.join("baseline.json")));

        std::fs::remove_dir_all(&dir).ok();
    }
}
