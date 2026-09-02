//! The gate's process-level contract: exit status and stdout body, together.
//!
//! The two statements that actually matter — print the body, exit with the code — live in a
//! binary, not in the library, so a `main` that printed the refusal to stderr or exited 0 on it
//! is invisible to the unit suite. These tests spawn the binary and read both.
//!
//! `.github/workflows/ai-gate.yml` redirects the gate's **stdout** into a file, posts that file
//! as a drift issue only when the step **failed**, and aborts when the file is empty. Those two
//! conditions are one contract: satisfying either alone posts nothing. So these tests assert
//! both on the same invocation rather than in separate cases.
//!
//! `ai-duel compare` is the binary under test because it is the only one of the three sharing
//! `emit_gate_verdict` that needs no card database and plays no games — it reads two report
//! files and prints a verdict. That makes this a millisecond test instead of a full suite run,
//! and it exercises the same shared emitter `ai-gate` and `ai-perf-gate` end in.
//!
//! The same subject widens to the CI contracts this crate's tests depend on: the hand-maintained
//! lists in `.github/workflows/` that must agree with what the code does, which nothing else
//! checks.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use phase_ai::duel_suite::run::{GameResult, MatchupResult, SuiteReport, SuiteStatus};
use phase_ai::duel_suite::Expected;

/// Build the fixture from the real types rather than hand-written JSON.
///
/// Serialising the actual structs cannot drift from the schema (`Expected` is internally tagged,
/// which a hand-written fixture gets wrong silently): a field added to `SuiteReport` breaks
/// compilation here rather than producing a fixture the binary rejects on a parse error instead
/// of on the contract under test, and the parse is exercised by the binary, not asserted here.
///
/// `games_per_matchup` is the workload knob; everything else is held equal so the refusal in
/// the first test can only come from that field.
fn report_json(games_per_matchup: usize) -> String {
    let report = SuiteReport {
        schema_version: 2,
        git_sha: None,
        card_data_hash: None,
        unix_timestamp_secs: 0,
        difficulty: "Medium".to_string(),
        games_per_matchup,
        base_seed: 7,
        results: vec![MatchupResult {
            matchup_id: "red-mirror".to_string(),
            exercises: Vec::new(),
            p0_label: "a".to_string(),
            p1_label: "b".to_string(),
            expected: Expected::Mirror { tolerance: 0.4 },
            p0_wins: 1,
            p1_wins: 0,
            draws: 0,
            games: vec![GameResult {
                seed: 1,
                winner: Some(0),
                turns: 10,
            }],
            total_turns: 10,
            total_duration_ms: 1,
            avg_turns: 10.0,
            avg_duration_ms: 1.0,
            status: SuiteStatus::Pass,
            fail_reason: None,
            attribution: None,
        }],
    };
    serde_json::to_string_pretty(&report).expect("serialize fixture")
}

fn write(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write fixture");
    path
}

fn run(baseline: &std::path::Path, current: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_ai-duel"))
        .args([
            "compare",
            &baseline.display().to_string(),
            &current.display().to_string(),
        ])
        .output()
        .expect("spawn ai-duel");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("phase-gate-cli-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// The refusal route, asserted as the PAIR the workflow needs. A non-zero exit with an empty
/// stdout aborts the publishing step ("failed without a drift report"); a populated stdout with
/// a zero exit never reaches it. Both, or the drift issue does not exist.
#[test]
fn a_refused_comparison_exits_nonzero_and_writes_its_reason_to_stdout() {
    let dir = tempdir("refuse");
    let baseline = write(&dir, "baseline.json", &report_json(10));
    let current = write(&dir, "current.json", &report_json(100));

    let (code, stdout, stderr) = run(&baseline, &current);

    assert_ne!(
        code, 0,
        "a refusal must fail the step; stdout was:\n{stdout}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "an empty report body aborts the publishing step; stderr was:\n{stderr}"
    );
    // The body must be the refusal, not merely non-empty — a table of PASSing rows with no
    // statement of what failed is the outcome this whole change exists to prevent.
    assert!(stdout.contains("comparison refused"), "stdout:\n{stdout}");
    assert!(stdout.contains("games_per_matchup"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("10") && stdout.contains("100"),
        "stdout:\n{stdout}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A report that cannot be READ must refuse on the same terms as one that cannot be COMPARED.
///
/// A refusal that reaches only stderr leaves the workflow's redirected stdout empty, and its
/// "failed without a drift report" abort fires instead of the refusal being posted. Both inputs
/// are covered because they are separate arms in the source — a fix applied to one and not the
/// other is exactly the shape of defect this file exists to catch.
///
/// Missing and malformed are both exercised because they take different `CompareError` variants
/// (`Io` vs `Parse`) to the same renderer, and a remedy keyed on only one of them would leave the
/// other with an empty body.
#[test]
fn an_unreadable_report_still_publishes_a_refusal_body() {
    for (case, make_bad) in [("missing", false), ("malformed", true)] {
        for bad_side in ["baseline", "current"] {
            let dir = tempdir(&format!("unreadable-{case}-{bad_side}"));
            let good = write(&dir, "good.json", &report_json(10));
            let bad = dir.join(format!("{bad_side}-bad.json"));
            if make_bad {
                std::fs::write(&bad, "{ this is not a suite report").expect("write malformed");
            }
            // PREMISE: the "missing" case really is missing, or it would be testing nothing.
            assert_eq!(bad.exists(), make_bad, "fixture for {case}/{bad_side}");

            let (baseline, current) = if bad_side == "baseline" {
                (bad.clone(), good.clone())
            } else {
                (good.clone(), bad.clone())
            };
            let (code, stdout, stderr) = run(&baseline, &current);

            assert_eq!(code, 2, "{case}/{bad_side} must exit 2; stderr:\n{stderr}");
            assert!(
                stdout.contains("Gate: comparison refused"),
                "{case}/{bad_side} must publish a refusal body on STDOUT, not stderr; \
                 stdout was {} bytes:\n{stdout}",
                stdout.len()
            );
            // The body must say more than the header — an envelope with no remedy is the same
            // empty-file problem wearing a title.
            assert!(
                stdout.contains("could not be read"),
                "{case}/{bad_side} body must carry the remedy; stdout:\n{stdout}"
            );
            // The side is what makes it actionable, and it lives on stderr by design.
            assert!(
                stderr.contains(bad_side),
                "{case}/{bad_side} stderr must name which report failed; stderr:\n{stderr}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}

/// Control arm. Without it every assertion above is satisfied by a binary that refuses
/// everything, which would be a worse regression than the one being fixed.
#[test]
fn a_comparable_pair_exits_zero_and_writes_a_table() {
    let dir = tempdir("accept");
    let baseline = write(&dir, "baseline.json", &report_json(10));
    let current = write(&dir, "current.json", &report_json(10));

    let (code, stdout, stderr) = run(&baseline, &current);

    assert_eq!(
        code, 0,
        "identical reports must compare clean; stderr:\n{stderr}"
    );
    assert!(stdout.contains("| red-mirror |"), "stdout:\n{stdout}");
    assert!(stdout.contains("compare: 0 FAIL"), "stdout:\n{stdout}");
    assert!(
        !stdout.contains("comparison refused"),
        "control arm must not refuse; stdout:\n{stdout}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Repository root, from the crate this test compiles in.
///
/// `CARGO_MANIFEST_DIR` points at source the shard's checkout provides, unlike `CARGO_BIN_EXE_*`,
/// which points at a build artifact the archive does not ship.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `.rs` file under `dir`, recursively. Callers pass directories they have confirmed exist.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read a directory that is_dir() reported") {
        let path = entry.expect("read directory entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Binaries named by a literal `CARGO_BIN_EXE_<name>` under `crates/*/tests` and `crates/*/benches`.
///
/// Cargo sets that variable only when building an integration test or a benchmark, so an
/// occurrence elsewhere would not compile. This is a LOWER BOUND on "every binary a test spawns":
/// a name assembled through `concat!` or supplied by a build script evades a textual scan.
fn bin_exe_consumers(root: &Path) -> BTreeSet<String> {
    const PREFIX: &str = "CARGO_BIN_EXE_";
    let mut files = Vec::new();
    // The one directory whose absence means the scanner is aimed wrong; a missing per-crate
    // `tests`/`benches` is the normal case (most crates have neither).
    for entry in fs::read_dir(root.join("crates")).expect("read crates/") {
        let crate_dir = entry.expect("read crates/ entry").path();
        for sub in ["tests", "benches"] {
            let dir = crate_dir.join(sub);
            if dir.is_dir() {
                rs_files(&dir, &mut files);
            }
        }
    }
    let mut names = BTreeSet::new();
    for path in files {
        let text = fs::read_to_string(&path).expect("read integration-test source");
        for (idx, _) in text.match_indices(PREFIX) {
            let name = binary_name(&text[idx + PREFIX.len()..]);
            if !name.is_empty() {
                names.insert(name);
            }
        }
    }
    names
}

/// The leading crate-name-shaped token of `rest`.
fn binary_name(rest: &str) -> String {
    rest.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// A commented-out YAML line. Both workflow scanners below skip these — an illustrative or
/// disabled command inside a comment is not a live site.
fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

const DEBUG_DIR: &str = "target/debug/";

/// `target/debug/<name>` occurrences on the non-comment `ci.yml` lines `keep` selects.
///
/// Shipping a binary to a shard is a CONJUNCTION over two independently editable sites: the
/// upload `path:` list carries the file, and the `chmod +x` line restores the executable bit
/// Actions artifacts drop. Neither implies the other, so the two are scanned separately and
/// never unioned. `match_indices`, not `find`: the `chmod` line names several binaries.
fn debug_binaries(ci_yml: &str, keep: impl Fn(&str) -> bool) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in ci_yml
        .lines()
        .filter(|line| !is_comment(line) && keep(line))
    {
        for (idx, _) in line.match_indices(DEBUG_DIR) {
            let name = binary_name(&line[idx + DEBUG_DIR.len()..]);
            if !name.is_empty() {
                names.insert(name);
            }
        }
    }
    names
}

/// Lines that ARE an upload `path:` entry: the whole trimmed content is one `target/debug/<name>`,
/// as a block-scalar item or as the single-entry inline form. Keyed on any mention instead, a
/// `run: ls -l target/debug/<name>` elsewhere in the file would enroll a binary the upload step
/// never carries, and the set-equality assertion would then confirm agreement about a binary that
/// never reaches the shard. Every command form (`chmod +x …`, `ls -l …`) leaves a prefix, so none
/// of them can pass this.
fn uploaded_binaries(ci_yml: &str) -> BTreeSet<String> {
    debug_binaries(ci_yml, |line| {
        let entry = line.trim();
        let entry = entry.strip_prefix("path:").map_or(entry, str::trim_start);
        entry
            .strip_prefix(DEBUG_DIR)
            .is_some_and(|name| name == binary_name(name))
    })
}

fn chmod_binaries(ci_yml: &str) -> BTreeSet<String> {
    debug_binaries(ci_yml, |line| line.contains("chmod"))
}

fn unshipped(consumers: &BTreeSet<String>, shipped: &BTreeSet<String>) -> Vec<String> {
    consumers.difference(shipped).cloned().collect()
}

/// The sharded CI jobs replay a nextest archive and never invoke Cargo, so every binary an
/// integration test spawns has to be hand-shipped to them by `ci.yml`. A name that reaches
/// neither list dies at `Command::spawn`, in the shard, with the rest of the suite green.
#[test]
fn every_binary_an_integration_test_spawns_is_shipped_to_the_test_shards() {
    let root = repo_root();
    let consumers = bin_exe_consumers(&root);
    assert!(
        !consumers.is_empty(),
        "no CARGO_BIN_EXE_ consumer found under crates/*/tests — the scanner or the tree moved"
    );

    let ci = fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci.yml");
    let uploaded = uploaded_binaries(&ci);
    let chmodded = chmod_binaries(&ci);
    assert!(
        !uploaded.is_empty(),
        "no uploaded binary found in ci.yml — the scanner or the upload step moved"
    );
    assert!(
        !chmodded.is_empty(),
        "no chmod'd binary found in ci.yml — the scanner or the chmod step moved"
    );

    assert!(
        unshipped(&consumers, &uploaded).is_empty(),
        "{:?} are spawned by an integration test but are not in ci.yml's `Upload test runtime \
         binaries` path: list. The shards replay an archive and never invoke cargo, so a \
         CARGO_BIN_EXE_ path that was not uploaded resolves to a file that does not exist.",
        unshipped(&consumers, &uploaded)
    );
    assert!(
        unshipped(&consumers, &chmodded).is_empty(),
        "{:?} are spawned by an integration test but are not on ci.yml's `chmod +x` line. \
         Actions artifacts preserve file contents but not executable bits, so an \
         uploaded-but-not-chmod'd binary reaches the shard and fails at spawn with \
         PermissionDenied.",
        unshipped(&consumers, &chmodded)
    );

    // The two ci.yml sites name the SAME binaries, in both directions. A chmod on a name the
    // upload omitted fails the step loudly; an uploaded-but-unchmodded name reaches the shard and
    // dies at spawn. Set against set, never against a literal, so a fourth consumer passes once
    // it is added to both sites and fails while it is missing from one.
    assert_eq!(
        uploaded, chmodded,
        "ci.yml's `Upload test runtime binaries` path: list and its `chmod +x` line must name the \
         same binaries"
    );

    // Refused end, withheld from ONE site: a name absent from either list alone must be named
    // by that leg and by no other. An implementation that unioned the two sites passes the
    // assertions above while shipping a PermissionDenied.
    let victim = consumers.iter().next().expect("non-empty above").clone();
    let mut degraded = chmodded.clone();
    degraded.remove(&victim);
    assert_eq!(unshipped(&consumers, &degraded), vec![victim.clone()]);
    assert!(unshipped(&consumers, &uploaded).is_empty());

    // Admitted end: containment, not equality — a shipped binary nothing spawns stays admitted.
    let spare = "a-binary-no-integration-test-spawns".to_string();
    let mut wider_uploaded = uploaded.clone();
    wider_uploaded.insert(spare.clone());
    let mut wider_chmod = chmodded.clone();
    wider_chmod.insert(spare);
    assert!(unshipped(&consumers, &wider_uploaded).is_empty());
    assert!(unshipped(&consumers, &wider_chmod).is_empty());
}

/// The two scanners must read DIFFERENT lines. The real lists are equal, so every set-level
/// assertion above survives a pair that aliased — this fixture is what does not.
#[test]
fn the_upload_and_chmod_scanners_partition_the_file() {
    const FIXTURE: &str = "  path: target/debug/alpha\n  run: chmod +x target/debug/beta\n";

    assert_eq!(
        uploaded_binaries(FIXTURE),
        BTreeSet::from(["alpha".to_string()])
    );
    assert_eq!(
        chmod_binaries(FIXTURE),
        BTreeSet::from(["beta".to_string()])
    );
}

/// A `target/debug/<name>` that is merely MENTIONED — an `ls`, a `cp`, a diagnostic — is not an
/// upload `path:` entry. Keyed on any mention, `spare` below joins BOTH sets: every subset
/// assertion holds and the two sets compare equal, so the suite stays green while `spare` is
/// never uploaded and its consumer dies at spawn in the shard.
#[test]
fn a_mentioned_binary_is_not_an_uploaded_one() {
    // The three real shapes: a block-scalar `path:` entry, a diagnostic that merely names a
    // binary, and the chmod line that names both.
    const FIXTURE: &str = "          path: |
            target/debug/shipped
        run: ls -l target/debug/spare
        run: chmod +x target/debug/shipped target/debug/spare
";

    assert_eq!(
        uploaded_binaries(FIXTURE),
        BTreeSet::from(["shipped".to_string()])
    );
    assert_eq!(
        chmod_binaries(FIXTURE),
        BTreeSet::from(["shipped".to_string(), "spare".to_string()])
    );
    assert_ne!(uploaded_binaries(FIXTURE), chmod_binaries(FIXTURE));

    // The consumer of the un-uploaded binary is named by the upload leg, and only by it.
    let consumers = BTreeSet::from(["spare".to_string()]);
    assert_eq!(
        unshipped(&consumers, &uploaded_binaries(FIXTURE)),
        vec!["spare".to_string()]
    );
    assert!(unshipped(&consumers, &chmod_binaries(FIXTURE)).is_empty());
}

/// Every `cargo ai-gate` invocation written down in `.github/workflows/`, as `(file, tokens)`.
///
/// `cargo ai-perf-gate` is a different binary with its own baseline, so the match requires a
/// non-`-` boundary after the name.
fn gate_invocations(root: &Path) -> Vec<(String, Vec<String>)> {
    const NEEDLE: &str = "cargo ai-gate";
    let mut out = Vec::new();
    // A missing directory means the scanner is aimed wrong; it must not yield an empty pass.
    for entry in fs::read_dir(root.join(".github/workflows")).expect("read .github/workflows") {
        let path = entry.expect("read workflow entry").path();
        if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let file = path
            .file_name()
            .expect("a file with an extension has a name")
            .to_string_lossy()
            .into_owned();
        let text = fs::read_to_string(&path).expect("read workflow");
        for line in text.lines().filter(|line| !is_comment(line)) {
            let invokes_gate = line
                .match_indices(NEEDLE)
                .any(|(idx, _)| !line[idx + NEEDLE.len()..].starts_with('-'));
            if invokes_gate {
                out.push((
                    file.clone(),
                    line.split_whitespace().map(str::to_string).collect(),
                ));
            }
        }
    }
    out
}

/// The token after `name`, when `name` is present.
fn flag<'a>(tokens: &'a [String], name: &str) -> Option<&'a str> {
    let idx = tokens.iter().position(|token| token == name)?;
    tokens.get(idx + 1).map(String::as_str)
}

/// Each invocation's disagreements with the baseline, one string per (invocation, field).
///
/// A field is compared case-insensitively because `AiDifficulty::from_label` folds case — and a
/// MISSING field is a finding too: an unspelled value comes from a `bin/ai_gate.rs` default, so
/// a change there would break the job at runtime with every test still green.
fn findings(invocations: &[(String, Vec<String>)], baseline: &SuiteReport) -> Vec<String> {
    let expected = [
        ("--games", baseline.games_per_matchup.to_string()),
        ("--seed", baseline.base_seed.to_string()),
        ("--difficulty", baseline.difficulty.clone()),
    ];
    let mut out = Vec::new();
    for (file, tokens) in invocations {
        for (name, want) in &expected {
            match flag(tokens, name) {
                None => out.push(format!(
                    "{file}: `{name}` is not spelled out; its value comes from a binary default \
                     nothing checks. Add `{name} {want}`."
                )),
                Some(got) if !got.eq_ignore_ascii_case(want) => out.push(format!(
                    "{file}: `{name} {got}`, but the baseline was recorded at `{want}`. The \
                     comparator refuses a pair whose workloads disagree, so this invocation can \
                     reach no verdict. Set `{name} {want}`."
                )),
                Some(_) => {}
            }
        }
    }
    out
}

/// The gate's workload must be written down where it is invoked, at the value the baseline was
/// recorded at. Anything else makes the job incapable of a verdict — the comparator refuses the
/// pair — and an unspelled field hides the dependency on a binary constant instead.
#[test]
fn every_workflow_gate_invocation_spells_the_baseline_workload() {
    let root = repo_root();
    let invocations = gate_invocations(&root);
    assert!(
        !invocations.is_empty(),
        "no `cargo ai-gate` invocation found under .github/workflows — the scanner or the \
         workflows moved"
    );

    for (file, tokens) in &invocations {
        assert!(
            flag(tokens, "--baseline").is_none(),
            "{file} names its own baseline; extend this check to resolve a baseline per \
             invocation before comparing any of them against the default one"
        );
    }

    let baseline = phase_ai::duel_suite::compare::load_report(
        &root.join("crates/phase-ai/baselines/suite-baseline.json"),
    )
    .expect("load the committed baseline through the production loader");

    let live = findings(&invocations, &baseline);
    assert!(
        live.is_empty(),
        "workflow gate invocations disagree with the baseline:\n{}",
        live.join("\n")
    );

    // Two invocations, one matching and one divergent, driven through the same call: the
    // findings must name the divergent one and only it. A file-level verdict fails here, and an
    // instrument that cannot produce a finding at all fails here too.
    let spell = |games: usize| {
        format!(
            "cargo ai-gate --games {games} --seed {} --difficulty {}",
            baseline.base_seed,
            baseline.difficulty.to_lowercase()
        )
    };
    let tokens = |line: String| line.split_whitespace().map(str::to_string).collect();
    let control = findings(
        &[
            (
                "matching.yml".to_string(),
                tokens(spell(baseline.games_per_matchup)),
            ),
            (
                "divergent.yml".to_string(),
                tokens(spell(baseline.games_per_matchup + 90)),
            ),
        ],
        &baseline,
    );
    let named: BTreeSet<&str> = control
        .iter()
        .map(|finding| finding.split(':').next().expect("a finding names its file"))
        .collect();
    assert_eq!(
        named,
        BTreeSet::from(["divergent.yml"]),
        "control must name exactly the divergent invocation; findings:\n{}",
        control.join("\n")
    );
}

/// A commented-out invocation is not an invocation. Without the skip, an illustrative
/// `# cargo ai-gate ...` line anywhere under `.github/workflows/` becomes a finding, and the
/// scanner starts dictating what the comments beside it are allowed to say.
#[test]
fn a_commented_out_gate_invocation_is_not_scanned() {
    let root = tempdir("commented-invocation");
    let workflows = root.join(".github/workflows");
    fs::create_dir_all(&workflows).expect("create workflow fixture dir");
    write(
        &workflows,
        "fixture.yml",
        "  # cargo ai-gate --games 100 --seed 7 --difficulty medium\n\
         run: cargo ai-gate --games 40 --seed 7 --difficulty medium\n",
    );

    let found = gate_invocations(&root);

    // PREMISE: the live line IS picked up, or the count below is satisfied by a dead scanner.
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(flag(&found[0].1, "--games"), Some("40"), "{found:?}");

    fs::remove_dir_all(&root).ok();
}
