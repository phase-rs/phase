//! The manifest schema and everything `validate()` can refuse before a single target runs.
//!
//! `Serialize` is derived alongside `Deserialize` purely so `block::digest` needs no
//! hand-written canonical serializer: declaration order IS the canonical order and JSON
//! escaping IS the injection safety.

use std::collections::BTreeMap;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::Abort;

/// probe-pin owns these libtest options; a manifest may not pass them through `[target].args`.
/// Compared against the option NAME (`--x=v` -> `--x`, any `-Z…` -> `-Z`), not the raw token.
const RESERVED: &[&str] = &[
    "--format",
    "-Z",
    "--nocapture",
    "--show-output",
    "--test-threads",
    "-q",
    "--quiet",
    "--exact",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunMode {
    RuntimeRead,
    Compiled,
}

impl std::fmt::Display for RunMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RuntimeRead => "runtime-read",
            Self::Compiled => "compiled",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterMatch {
    Substring,
    Exact,
}

impl std::fmt::Display for FilterMatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Substring => "substring",
            Self::Exact => "exact",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub mode: RunMode,
    pub package: String,
    pub test: String,
    pub filter: String,
    pub filter_match: FilterMatch,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_env")]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

impl Target {
    /// Every argv token this manifest contributes to the target's command line, in argv order,
    /// tagged with the field that contributed it.
    ///
    /// THE single authority for "which manifest values reach the target's argv". `isolate::argv`
    /// builds the real argv from this list and `validate` checks RESERVED against it, so the two
    /// cannot drift: a future field that feeds argv is validated the moment it is added here,
    /// and is unreachable by `isolate::argv` until it is. `filter` was the miss that made this
    /// necessary — it is argv token 0, where libtest's getopts parses it as a FLAG if it looks
    /// like one, and only `args` was ever checked (measured: `filter = "--bench"`, exit 0, a
    /// green `mount reached: 1 file(s)` row for a run whose every test body was skipped).
    pub fn manifest_argv(&self) -> Vec<(&'static str, &str)> {
        std::iter::once(("[target].filter", self.filter.as_str()))
            .chain(self.args.iter().map(|a| ("[target].args", a.as_str())))
            .collect()
    }
}

fn default_env() -> BTreeMap<String, String> {
    BTreeMap::from([("RUST_MIN_STACK".to_string(), "16777216".to_string())])
}

fn default_timeout() -> u64 {
    300
}

fn one() -> u32 {
    1
}

/// The largest `Prepend` pad probe-pin will materialize, 64 MiB. Not a tuned number: the pad is
/// held in memory, written to disk and read back by the target as source text, and the largest
/// pad any manifest in this repository declares is 200 × 14 B = 2.8 kB, so this is four orders
/// of magnitude of headroom over the shape it exists to allow and still refuses the shape that
/// killed the process.
pub const MAX_PREPEND_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub file: String,
    pub marker: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Mutation {
    Replace {
        file: String,
        find: String,
        replace: String,
    },
    Prepend {
        files: Vec<String>,
        text: String,
        #[serde(default = "one")]
        repeat: u32,
    },
}

impl Mutation {
    pub fn files(&self) -> Vec<&str> {
        match self {
            Self::Replace { file, .. } => vec![file.as_str()],
            Self::Prepend { files, .. } => files.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssertCount {
    pub file: String,
    pub text: String,
    pub count: usize,
}

/// `Pass {}` and not a unit variant: a unit variant defeats `deny_unknown_fields`, so a stray
/// `anchor` under `outcome = "pass"` would be silently accepted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase", deny_unknown_fields)]
pub enum Expect {
    Pass {},
    Fail { anchor: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub id: String,
    #[serde(default)]
    pub claim: String,
    #[serde(default, rename = "mutation")]
    pub mutations: Vec<Mutation>,
    #[serde(default, rename = "assert_count")]
    pub assert_counts: Vec<AssertCount>,
    pub expect: Expect,
}

impl Probe {
    /// Every file this probe mutates, in first-touch order — the fingerprint set of §7 step 6.
    pub fn touched(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in &self.mutations {
            for f in m.files() {
                if !out.iter().any(|p| p == f) {
                    out.push(f.to_string());
                }
            }
        }
        out
    }

    pub fn anchors(&self) -> &[String] {
        match &self.expect {
            Expect::Pass {} => &[],
            Expect::Fail { anchor } => anchor,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    pub id: String,
    pub pattern: String,
    pub paths: Vec<String>,
    pub sentence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub target: Target,
    pub output: Output,
    #[serde(default, rename = "probe")]
    pub probes: Vec<Probe>,
    #[serde(default, rename = "projection")]
    pub projections: Vec<Projection>,
}

impl Manifest {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("probe-pin: cannot read manifest {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("probe-pin: cannot parse manifest {}", path.display()))
    }

    /// The zero-mutation probe. `validate()` has already proved there is exactly one.
    pub fn control(&self) -> &Probe {
        self.probes
            .iter()
            .find(|p| p.mutations.is_empty())
            .expect("validate() proved exactly one zero-mutation probe exists")
    }
}

/// The anchor line-number lint — the invariant the deleted pad subsystem approximated, at
/// validation time and at zero target runs. Three forms:
/// `\bline\s*[:=]?\s*\d` ∪ `\.[A-Za-z]{1,5}:\d+` ∪ `:\d+:\d+`.
///
/// Named residual (docs/probe-pin.md): a positional integer that is syntactically
/// indistinguishable from a legitimate count — `("…/engine.rs", 11492)` and `L11492` — is
/// accepted, because every regex that rejects it also rejects P9's real shipping anchor.
pub fn embeds_line_number(anchor: &str) -> bool {
    fn is_word(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
    }
    let b = anchor.as_bytes();
    for i in 0..b.len() {
        if b[i..].starts_with(b"line") && (i == 0 || !is_word(b[i - 1])) {
            let mut j = i + 4;
            while b.get(j).is_some_and(u8::is_ascii_whitespace) {
                j += 1;
            }
            if matches!(b.get(j), Some(b':') | Some(b'=')) {
                j += 1;
            }
            while b.get(j).is_some_and(u8::is_ascii_whitespace) {
                j += 1;
            }
            if b.get(j).is_some_and(u8::is_ascii_digit) {
                return true;
            }
        }
        if b[i] == b'.' {
            let mut j = i + 1;
            while j - i <= 5 && b.get(j).is_some_and(u8::is_ascii_alphabetic) {
                j += 1;
            }
            if j > i + 1 && b.get(j) == Some(&b':') && b.get(j + 1).is_some_and(u8::is_ascii_digit)
            {
                return true;
            }
        }
        if b[i] == b':' {
            let mut j = i + 1;
            while b.get(j).is_some_and(u8::is_ascii_digit) {
                j += 1;
            }
            if j > i + 1 && b.get(j) == Some(&b':') && b.get(j + 1).is_some_and(u8::is_ascii_digit)
            {
                return true;
            }
        }
    }
    false
}

/// A probe id is joined onto the scratch dir as ONE path segment, so it is constrained by an
/// ACCEPTED charset, never by a rejected one: `..`, `.`, `/` and a leading `/` are each a way
/// out of the container, and a blacklist that names them is a list of the ways someone has
/// thought of. Ids are also the block's row keys and the digest's ordering keys.
pub fn is_plain_name(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Same rule, one level up: a manifest path is joined onto the workspace root AND onto the
/// scratch dir, so every component must be a plain name — no root, no prefix, no `..`, no `.`.
///
/// LEXICAL only, and that is all it is used for now: `Path::components()` says nothing about
/// what a component RESOLVES to. It is the pre-filter that gives the actionable `..` message;
/// `resolve_contained` is the containment assertion.
pub fn is_workspace_relative(path: &str) -> bool {
    !path.is_empty()
        && std::path::Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Realpath containment: `base.join(key)` must RESOLVE inside `base`. Returns the joined path,
/// or the reason it escapes.
///
/// The lexical check above accepts every path whose components are `Normal`, which an in-tree
/// SYMLINK satisfies while resolving anywhere on the filesystem (measured: `[output].file`
/// through one, exit 0, the block spliced outside the repository). `isolate::scratch_dir`
/// already states containment the only way that holds — canonicalize, then `starts_with` — and
/// this is that same assertion applied to the keys a manifest supplies, so both path
/// directions (into the workspace, into the scratch dir) are one function.
///
/// The named file need not exist: `[output].file` may be created and a mutant's destination
/// never exists yet. So what is canonicalized is the deepest ancestor that resolves — nothing
/// below it exists, so nothing below it can redirect the write. The one exception is a name
/// that EXISTS but does not resolve, i.e. a dangling symlink, which `canonicalize` cannot see
/// and `fs::write` follows straight out of the tree; it is refused by name.
pub fn resolve_contained(base: &std::path::Path, key: &str) -> Result<std::path::PathBuf, String> {
    if !is_workspace_relative(key) {
        return Err(format!(
            "'{key}' is not a relative path with no '..' component"
        ));
    }
    let base_real = base
        .canonicalize()
        .map_err(|e| format!("{} cannot be resolved: {e}", base.display()))?;
    let full = base_real.join(key);
    let mut probe = full.as_path();
    let resolved = loop {
        match probe.canonicalize() {
            Ok(real) => break real,
            Err(_) if std::fs::symlink_metadata(probe).is_ok() => {
                return Err(format!(
                    "'{key}' passes through {}, a symlink whose target does not exist — a write follows it, and probe-pin cannot prove where to",
                    probe.display()
                ))
            }
            Err(_) => {
                probe = probe
                    .parent()
                    .ok_or_else(|| format!("'{key}' has no resolvable ancestor"))?;
            }
        }
    };
    if !resolved.starts_with(&base_real) {
        return Err(format!(
            "'{key}' resolves to {}, which is outside {}",
            resolved.display(),
            base_real.display()
        ));
    }
    Ok(full)
}

/// The option NAME a token carries: `--test-threads=4` -> `--test-threads`, `-Zfoo` -> `-Z`.
pub fn option_name(arg: &str) -> &str {
    if arg.starts_with("-Z") {
        return "-Z";
    }
    arg.split_once('=').map_or(arg, |(k, _)| k)
}

/// Every manifest value that reaches a process argv POSITIONALLY, tagged with its field.
/// probe-pin shells three programs that take manifest values — the target (`isolate::argv`),
/// cargo (`target::resolve`) and ast-grep (`project::count`) — and an operand is where a
/// leading `-` is reinterpreted as an option by all three grammars. Both measured:
/// `[target].filter = "--bench"` ran zero test bodies while libtest still reported test_count 1,
/// and `[[projection]].paths = ["-h"]` made ast-grep print its HELP, which probe-pin counted
/// into a rendered "named at 30 sites" sentence at exit 0.
///
/// Three manifest values are deliberately absent. `[target].args` is the one field whose
/// contract IS flags — it is checked against `RESERVED` instead. `[target].package`,
/// `[target].test` and `[[projection]].pattern` are never operands: each is the VALUE of a
/// preceding flag, so a `-`-leading one is refused by the tool itself before any verdict exists
/// (measured: `cargo test -p --config --test …` exits 2 via `TargetBuildFailed`).
pub fn positional_argv_values(m: &Manifest) -> Vec<(String, &str)> {
    let mut out = vec![("[target].filter".to_string(), m.target.filter.as_str())];
    for pj in &m.projections {
        for path in &pj.paths {
            out.push((
                format!("[[projection]] '{}' paths entry", pj.id),
                path.as_str(),
            ));
        }
    }
    out
}

/// The manifest's path RELATIVE to the workspace root — the string that reaches the BEGIN line
/// AND the digest, so an absolute one would pin a block no other checkout can reproduce.
///
/// BOTH sides are canonicalized, like every other containment assertion in this crate:
/// `resolve_contained` canonicalizes `base`, `isolate::scratch_dir` canonicalizes both sides.
/// A prefix comparison between a resolved path and an unresolved base is a LEXICAL comparison
/// wearing a realpath's clothes, and its failure mode is a refusal no manifest edit can satisfy —
/// "manifest … is not inside the workspace root" for a manifest that is. `target::workspace_root`
/// already canonicalizes at the producer; this is the same rule stated at the guard, where it is
/// measurable (`pure_logic::manifest_rel_compares_realpaths`).
pub fn workspace_relative(
    manifest_path: &std::path::Path,
    root: &std::path::Path,
) -> anyhow::Result<String> {
    let real_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    manifest_path
        .canonicalize()
        .ok()
        .and_then(|p| {
            p.strip_prefix(&real_root)
                .ok()
                .map(|rel| rel.display().to_string())
        })
        .with_context(|| {
            format!(
                "probe-pin: manifest {} is not inside the workspace root {}. Its path is stamped into the block's BEGIN line and into the digest, so an absolute one would pin a block that no other checkout can reproduce. Move the manifest into the workspace and name it relative to the root. Aborting.",
                manifest_path.display(),
                root.display()
            )
        })
}

/// Every manifest-supplied string that is RENDERED INTO the block, tagged with its field and in
/// render order. The third single-authority enumeration, beside `Target::manifest_argv` (argv)
/// and `isolate::is_probe_pin_owned_env` (environment) — and `tests/pure_logic.rs`'s
/// `block_text_surface_is_one_authority` measures the enumeration against what `block::render`
/// actually emits, so a field that starts reaching the block is covered or the census fails.
///
/// The block is a LINE-oriented, marker-delimited artifact: `block::locate` counts
/// `<marker>:BEGIN` and `<marker>:END` per line, and `--check` re-verifies exactly the span
/// between them. A value carrying a newline or a marker tag therefore does not describe the
/// artifact, it RESHAPES it. Measured on the shipping binary: `anchor = ["PROBE-PIN:END"]`
/// rendered into the firing-assertion cell, `run --write` exited 0, and the spliced file then
/// carried two END markers — every later `check` aborts `MarkerNotUnique` (found 1/2), so the
/// pin can never be re-measured again without a hand edit.
///
/// `manifest_rel` is in the list because it is stamped on the BEGIN line: it is not a manifest
/// VALUE, but it is a caller-supplied string that reaches the same surface.
///
/// Mutation paths are checked whole, though `block::mutation_cell` renders only their file
/// name — over-refusing a directory component costs nothing and keeps this list readable as
/// "the fields", not "the substrings of the fields".
pub fn rendered_block_values<'a>(m: &'a Manifest, manifest_rel: &'a str) -> Vec<(String, &'a str)> {
    let mut out = vec![
        ("the manifest path".to_string(), manifest_rel),
        ("[output].marker".to_string(), m.output.marker.as_str()),
    ];
    for p in &m.probes {
        out.push((format!("[[probe]] '{}' id", p.id), p.id.as_str()));
        for file in p.mutations.iter().flat_map(Mutation::files) {
            out.push((format!("[[probe]] '{}' mutation file", p.id), file));
        }
        for anchor in p.anchors() {
            out.push((format!("[[probe]] '{}' anchor", p.id), anchor.as_str()));
        }
    }
    for pj in &m.projections {
        out.push((
            format!("[[projection]] '{}' sentence", pj.id),
            pj.sentence.as_str(),
        ));
    }
    out
}

/// §7 step 1b. The block-text refusals. Split from `validate` for the same reason
/// `validate_paths` is — it needs a value step 1 does not have, here the workspace-relative
/// manifest path — and it runs in the same place, before the target binary is resolved, so a
/// refusal still costs zero target runs.
pub fn validate_block_text(m: &Manifest, manifest_rel: &str) -> anyhow::Result<()> {
    // The marker is the block's only structural tag, so it gets the ACCEPTED-charset treatment
    // a probe id gets rather than a list of the values someone thought of. Measured on the
    // shipping binary: `marker = ""` was accepted, and its `:BEGIN`/`:END` tags then matched the
    // `PROBE-PIN:BEGIN`/`PROBE-PIN:END` lines of an UNRELATED manifest's committed block —
    // `run --write` exited 0 and replaced that block with one keyed on a tag so short that any
    // line containing it is a match.
    if !is_plain_name(&m.output.marker) {
        bail!("probe-pin: [output].marker '{}' is not a plain name. The marker IS the block's structure: probe-pin locates the pin by counting lines containing '<marker>:BEGIN' and '<marker>:END', so a blank or whitespace-only marker degenerates to ':BEGIN'/':END', which matches any line that happens to contain them — measured: a blank marker spliced over an unrelated manifest's committed block at exit 0. Use ASCII letters, digits, '_' and '-' (conventionally PROBE-PIN). Aborting.", m.output.marker);
    }
    let (begin_tag, end_tag) = (
        format!("{}:BEGIN", m.output.marker),
        format!("{}:END", m.output.marker),
    );
    for (field, value) in rendered_block_values(m, manifest_rel) {
        if let Some(c) = value.chars().find(|c| c.is_control()) {
            bail!("probe-pin: {field} = {value:?} carries the control character {c:?}. It is rendered into the block, which is a LINE-oriented artifact — a newline splits one measured row into two, and every other control character lands verbatim in a committed file that `check` must re-measure byte for byte. Anchor on printable text. Aborting.");
        }
        if value.contains(&begin_tag) || value.contains(&end_tag) {
            bail!("probe-pin: {field} = {value:?} contains the marker tag '{begin_tag}' or '{end_tag}'. probe-pin finds the block by counting those tags, so rendering one INSIDE the block writes a second marker: measured on the shipping binary, `run --write` exited 0 and the spliced file then held two END markers, after which every `check` aborts with 'expected exactly one {} pair … found 1/2' and the pin can only be recovered by editing the file by hand. Anchor on text that does not contain the marker. Aborting.", m.output.marker);
        }
    }
    Ok(())
}

/// §7 step 1b. The path refusals that need the workspace ROOT, which step 1 does not have.
///
/// Split from `validate` by necessity, not by taste: containment is a realpath assertion and a
/// realpath needs the base. It runs the moment the root is known and BEFORE the target binary
/// is resolved, so a refusal still costs zero target runs — the property step 1 had.
pub fn validate_paths(m: &Manifest, root: &std::path::Path) -> anyhow::Result<()> {
    if let Err(why) = resolve_contained(root, &m.output.file) {
        bail!("probe-pin: [output].file '{}' is not workspace-relative: {why}. The block is resolved against the workspace root, so a path that leaves it — through '..', a root, a prefix, or a SYMLINK whose components are all ordinary names — writes the pin outside the repository, where `probe-pin check` and CI can never re-measure it. Name it relative to the workspace root. Aborting.", m.output.file);
    }
    for p in &m.probes {
        for file in p
            .mutations
            .iter()
            .flat_map(Mutation::files)
            .chain(p.assert_counts.iter().map(|a| a.file.as_str()))
        {
            if let Err(why) = resolve_contained(root, file) {
                bail!("probe-pin: {} names '{file}', which is not a workspace-relative path: {why}. Manifest paths are read from the workspace root and materialized under probe-pin's scratch directory; one that resolves outside the root reads a file this pin does not measure. Aborting.", p.id);
            }
        }
    }
    // The fourth path producer, and the one that reaches a DIFFERENT program: `project::count`
    // runs ast-grep with `current_dir(root)` and these paths as its operands, so a path that
    // resolves outside the root renders a projection sentence counting code this repository does
    // not contain — a measured number about a tree the pin does not pin.
    for pj in &m.projections {
        for path in &pj.paths {
            if let Err(why) = resolve_contained(root, path) {
                bail!("probe-pin: [[projection]] '{}' scans '{path}', which is not a workspace-relative path: {why}. A projection counts sites in the tree this manifest pins; a path that resolves outside the workspace root renders a sentence about code no checkout of this repository can re-measure. Aborting.", pj.id);
            }
        }
    }
    Ok(())
}

/// §7 step 1. Everything refusable before the tree is touched, a target is built, or the
/// workspace root is even known. The path keys get their LEXICAL refusal here and their
/// containment refusal in `validate_paths`, which needs the root.
pub fn validate(m: &Manifest) -> anyhow::Result<()> {
    if m.version != 1 {
        bail!(
            "probe-pin: manifest version = {} is unknown; this probe-pin speaks schema version 1 only. Aborting.",
            m.version
        );
    }
    if m.target.mode != RunMode::RuntimeRead {
        return Err(Abort::UnsupportedMode {
            mode: m.target.mode,
        }
        .into());
    }
    // Every token the manifest puts on the target's argv, not just `[target].args` — one
    // enumeration, shared with `isolate::argv`, so a field cannot reach the argv unvalidated.
    for (field, arg) in m.target.manifest_argv() {
        if RESERVED.contains(&option_name(arg)) {
            return Err(Abort::ReservedArg {
                field: field.to_string(),
                arg: arg.to_string(),
            }
            .into());
        }
    }
    // What an OPERAND is, stated positively for every field that becomes one — a filter is a
    // substring of a test name, a projection path is a directory. Not a list of the flags
    // someone thought of: `--bench` is deliberately NOT in RESERVED, because enumerating hostile
    // flags is the shape that already failed twice in this crate, and the execution floor in
    // `verdict::observe` is what generalizes over the ones nobody named.
    for (field, value) in positional_argv_values(m) {
        if value.starts_with('-') {
            bail!("probe-pin: {field} = \"{value}\" begins with '-'. probe-pin passes it to a program as an OPERAND, where every argv grammar it shells reads a leading '-' as an option instead — measured: `[target].filter = \"--bench\"` ran zero test bodies while libtest still reported test_count 1, and `[[projection]].paths = [\"-h\"]` made ast-grep print its help, which probe-pin counted into a rendered 30-site sentence at exit 0. A filter is a SUBSTRING of a test name; a projection path is a DIRECTORY. Name one. Aborting.");
        }
    }
    // The same positive shape, applied to the empty end of the range: a filter is a substring
    // that NAMES a test, and "" names nothing. It is refused here rather than left to the
    // execution floor because the two `filter_match` modes fail in opposite directions and
    // neither reports the manifest defect. Measured with `filter_match = "substring"`: the run
    // widens to the whole target binary while the block still renders a row keyed on one named
    // filter — caught in that probe only because a widened test happened to fail, which made the
    // control abort ("the instrument is broken"), not because anything checked the filter. With
    // `filter_match = "exact"` it matches nothing, and the floor then reports an execution
    // failure — the right refusal for the wrong reason, blaming the target for a manifest typo.
    if m.target.filter.trim().is_empty() {
        bail!("probe-pin: [target].filter is blank. A filter is a SUBSTRING that names the test a probe measures, and \"\" names nothing: with filter_match = \"substring\" libtest matches EVERY test in the target binary, so the probe measures the whole suite while the block renders a row keyed on one named target (measured — the widened run is only caught when one of those tests happens to fail, which reports as a broken control, not as a manifest defect); with filter_match = \"exact\" it matches nothing and the execution floor blames the target for what is a typo here. Name the test. Aborting.");
    }
    // The manifest may not override an environment variable probe-pin itself sets. The refused
    // set is derived from what `isolate` actually sets, not restated here: `PATH` reaches the
    // `unshare … bash -c` script, where it resolves the `mount` and `cmp` whose readback is the
    // only reason a `pass` verdict means anything.
    for key in m.target.env.keys() {
        if crate::isolate::is_probe_pin_owned_env(key) {
            bail!("probe-pin: [target].env sets '{key}', which probe-pin itself sets for the target run. PATH and HOME are constructed so two developers pin the same capture bytes — and PATH resolves the `mount`, `cmp` and `timeout` the isolation script runs, so overriding it renders a green 'mount reached' row for a probe whose mutant was never mounted and never compared (measured). PP_MOUNTS, PP_MUTANT_*, PP_TARGET_*, TIMEOUT and TESTBIN are the script's own inputs. Remove it; RUST_MIN_STACK is the one probe-pin-set variable a manifest may raise. Aborting.");
        }
    }
    if m.target.timeout_secs == 0 {
        bail!("probe-pin: [target].timeout_secs = 0 disables `timeout` entirely — `timeout -k 5 0` runs the child to completion, so a hung target would never be killed and never named. Set a positive value. Aborting.");
    }
    // The block is the artifact this whole pipeline exists to produce, and `--check` re-verifies
    // it by resolving `[output].file` against the workspace root. `root.join("../x")` escapes,
    // and a pin written outside the workspace is a pin no checkout and no CI job can ever
    // re-measure — the verification loop it was rendered for cannot reach it. Measured: exit 0,
    // block spliced into a file outside the root. Same rule already governs every mutation path.
    if !is_workspace_relative(&m.output.file) {
        bail!("probe-pin: [output].file '{}' is not workspace-relative. The block is resolved against the workspace root, so a path with '..', a root, or a prefix writes the pin outside the repository — where `probe-pin check` and CI can never re-measure it, which is the one thing a pin exists to allow. Name it relative to the workspace root. Aborting.", m.output.file);
    }
    if m.probes.iter().filter(|p| p.mutations.is_empty()).count() != 1 {
        return Err(Abort::ControlMissing.into());
    }
    for (i, p) in m.probes.iter().enumerate() {
        if !is_plain_name(&p.id) {
            bail!("probe-pin: probe id '{}' is not a plain name. A probe id is joined onto probe-pin's scratch directory as ONE path segment, so a separator or a '..' in it materializes the mutant outside that directory — inside the tree probe-pin is measuring, which is the one thing this tool must never write. Use ASCII letters, digits, '_' and '-'. Aborting.", p.id);
        }
        if m.probes[..i].iter().any(|q| q.id == p.id) {
            bail!("probe-pin: duplicate probe id '{}'. Probe ids are the block's row keys and the digest's ordering; they must be unique. Aborting.", p.id);
        }
        // The control materializes no mutant, and `assert_count` is checked against the mutant
        // text — so one declared here is pinned into the digest and never evaluated. Measured:
        // a control carrying `count = 99` of a string that occurs nowhere exited 0, green.
        if p.mutations.is_empty() && !p.assert_counts.is_empty() {
            bail!("probe-pin: {} declares assert_count(s) with no mutation. assert_count is checked against the MUTANT text, and a probe with no mutation is the control — it never materializes one, so these would be pinned into the block's digest without ever being evaluated. Move them to the probe whose mutation they describe. Aborting.", p.id);
        }
        // An empty file list is not a mutation: it seeds no file, so `mutate::apply`'s per-file
        // no-op gate never executes, nothing is mounted, and the probe's verdict is about the
        // UNMODIFIED tree — the control's job, rendered as a `pass` row that claims a mutant was
        // visible. Refused per MUTATION so it cannot ride along beside one that names a file.
        for (index, mutation) in p.mutations.iter().enumerate() {
            if mutation.files().is_empty() {
                bail!("probe-pin: {} mutation[{index}] names no file. A mutation with an empty file list mutates nothing and mounts nothing, so this probe would measure the unmodified tree and render it as a mutant's verdict. Name the file(s) to mutate, or delete the probe. Aborting.", p.id);
            }
            // The size of the mutant is a manifest-controlled PRODUCT, and the only one:
            // `Replace` is bounded by the file it edits, `Prepend` is `text.len() × repeat`
            // with `repeat` a full u32. Measured on the shipping binary: `repeat = 4294967295`
            // of a 14-byte pad asked `str::repeat` for 60,129,542,130 bytes, and an allocation
            // failure is not a panic — `handle_alloc_error` ABORTS, so probe-pin died with
            // SIGABRT (exit 134, core dumped) outside its documented 0/1/2/101 exit contract,
            // after a full control run had already been spent.
            if let Mutation::Prepend { text, repeat, .. } = mutation {
                let bytes = text.len().saturating_mul(*repeat as usize);
                if bytes > MAX_PREPEND_BYTES {
                    bail!("probe-pin: {} mutation[{index}] prepends {} bytes ({} × repeat {repeat}), over probe-pin's {MAX_PREPEND_BYTES}-byte cap. The pad is materialized in memory, written to the scratch dir, bind-mounted and then READ by the target, so this is a mutant no probe can measure — and past the allocator's limit it is not even a refusal: probe-pin aborts on allocation failure (measured: 60,129,542,130 bytes requested, SIGABRT). The shipping pads in this repository are ~2.8 kB. Lower `repeat`, or shorten `text`. Aborting.", p.id, bytes, text.len());
                }
            }
        }
        for file in p
            .mutations
            .iter()
            .flat_map(Mutation::files)
            .chain(p.assert_counts.iter().map(|a| a.file.as_str()))
        {
            if !is_workspace_relative(file) {
                bail!("probe-pin: {} names '{file}', which is not a workspace-relative path. Manifest paths are joined onto the workspace root AND onto probe-pin's scratch directory, so an absolute path or a '..' component reads outside the workspace and materializes the mutant outside the scratch directory — inside the tree probe-pin is measuring. Use a path relative to the workspace root, with no '..' component. Aborting.", p.id);
            }
        }
        // `assert_count` is evaluated against the MUTANT text, inside `mutate::apply`, so the
        // only file it can be evaluated on is one this probe mutates. Naming any other file
        // used to fall back to reading the PRISTINE tree while the failure message said "in
        // the MUTANT of" — the assertion ran, but on text no mutation had touched, so an author
        // reading that message believed they had pinned post-mutation state. Refusing is the
        // half that stays true later: it is the same shape as the control refusal above (a
        // declaration pinned into the digest that its gate never sees), and it costs zero
        // target runs, where a corrected message would still have pinned pristine text.
        for ac in &p.assert_counts {
            if !p.touched().contains(&ac.file) {
                bail!("probe-pin: {} declares an assert_count on '{}', which this probe does not mutate. assert_count is checked against the MUTANT text; a file with no mutation has no mutant, so this would assert against the pristine tree — a claim about post-mutation state, measured on pre-mutation text. Mutate that file in this probe, or move the assert_count to the probe that does. Aborting.", p.id, ac.file);
            }
        }
        if let Expect::Fail { anchor } = &p.expect {
            if anchor.is_empty() {
                bail!("probe-pin: {} expects outcome = \"fail\" with an empty anchor list. A failure with no anchor pins nothing — any failure would satisfy it. Aborting.", p.id);
            }
            // The identical hazard, one character away: anchors are matched with `contains`,
            // and EVERY capture contains "". The empty list was refused for exactly this reason
            // while `anchor = [""]` was accepted — measured: exit 0, a green `fail` row whose
            // firing-assertion cell is BLANK, which reads to a reviewer as "no anchor was
            // needed" rather than "any failure satisfies this". `" "` behaves the same, hence
            // the trim.
            if let Some(blank) = anchor.iter().find(|a| a.trim().is_empty()) {
                bail!("probe-pin: {} declares the anchor {blank:?}, which is empty after trimming. Anchors are matched with `contains`, and every capture contains the empty string — so this anchor is satisfied by ANY failure, exactly like the empty anchor list, and it renders as a blank firing-assertion cell that reads as 'no anchor was needed'. Anchor on the text of the assertion this probe pins. Aborting.", p.id);
            }
        }
        for a in p.anchors() {
            if embeds_line_number(a) {
                return Err(Abort::AnchorEmbedsLineNumber {
                    probe: p.id.clone(),
                    anchor: a.clone(),
                }
                .into());
            }
        }
        if let Some(twin) = m.probes[..i]
            .iter()
            .find(|q| !q.anchors().is_empty() && q.anchors() == p.anchors())
        {
            return Err(Abort::AnchorNotDiscriminating {
                probe: p.id.clone(),
                collides_with: twin.id.clone(),
            }
            .into());
        }
    }
    // A `[[projection]]` is a producer of block rows exactly like a `[[probe]]`, and it had only
    // its path keys checked. The three refusals below are the projection spelling of three the
    // probe loop above already makes — a row key must be a plain name and unique, and a
    // declaration that cannot carry a measurement must not reach the block.
    for (i, pj) in m.projections.iter().enumerate() {
        if !is_plain_name(&pj.id) {
            bail!("probe-pin: [[projection]] id '{}' is not a plain name. A projection id is a block row key and one of the digest's ordering keys, exactly like a probe id. Use ASCII letters, digits, '_' and '-'. Aborting.", pj.id);
        }
        // Measured on the shipping binary: two projections sharing an id rendered the FIRST
        // one's count under BOTH sentences, because the count is looked up by id — `Abort` was
        // measured at 1 site and the block said 7, which is the other pattern's number.
        if m.projections[..i].iter().any(|q| q.id == pj.id) {
            bail!("probe-pin: duplicate [[projection]] id '{}'. Counts are looked up by id when the block is rendered and when the digest is computed, so the second projection would render the FIRST one's number under its own sentence — measured: a pattern with 1 match rendered as 7. Projection ids must be unique. Aborting.", pj.id);
        }
        // Measured: `paths = []` made ast-grep scan its whole CWD (the workspace root) and
        // probe-pin rendered "named at 15 sites" for a projection whose declared scan set was
        // empty, where the paths this crate's own projection names hold 7.
        if pj.paths.is_empty() {
            bail!("probe-pin: [[projection]] '{}' names no path. ast-grep with no path operand scans probe-pin's whole working directory — the workspace root — so the rendered count would be a measurement of a tree this projection never declared (measured: 15 sites rendered for an empty path list, against 7 in the paths the manifest meant). Name the directories to scan. Aborting.", pj.id);
        }
        // The projection half of this crate's thesis: prose in the block is a TRANSCRIPTION of
        // a measured number. A sentence with no substitution point is an assertion, and it is
        // rendered inside the span `check` re-verifies, where it reads as measured.
        if !pj.sentence.contains("{count}") {
            bail!("probe-pin: [[projection]] '{}' has a sentence with no '{{count}}' placeholder: {:?}. The count is substituted into that placeholder, so this sentence would be rendered into the block — inside the span `check` re-verifies — carrying no measured number at all, which is the one thing this tool exists to refuse. Put {{count}} where the number belongs. Aborting.", pj.id, pj.sentence);
        }
    }
    Ok(())
}
