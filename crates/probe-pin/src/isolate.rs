//! §7 steps 5, 6 and 8: the scratch dir, the fingerprints, and the isolated target run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::manifest::{FilterMatch, Target};
use crate::mutate::Mount;
use crate::{sha256_hex, Abort};

/// The whole script. Four probe-pin-owned variables reach it through the child's ENVIRONMENT
/// (`$MUTANT`/`$TARGET` indexed per mount, `$TIMEOUT`, `$TESTBIN`), never as positionals — so
/// `"$@"` is exactly the target's argv and shell quoting never touches user-supplied `args`.
///
/// The `cmp -s` readback is the whole reason a `pass` verdict means anything: it proves the
/// bytes the target can see at `$TARGET` are the mutant's, inside the namespace, after the
/// mount. `set -eu` makes a missing `timeout`/`mount`/`cmp` a non-zero exit, never a silent
/// unmounted run.
///
/// Two exit codes are RESERVED for the script itself, and `verdict::observe` classifies both
/// before it ever looks at the record stream: 97 = the mount readback failed, 96 = the script
/// failed before the target was reached at all.
pub const SCRIPT: &str = r#"set -eu
trap 'printf "probe-pin: ISOLATION SCRIPT FAILED before the target ran\n" >&2; exit 96' EXIT
i=0
while [ "$i" -lt "$PP_MOUNTS" ]; do
  m="PP_MUTANT_$i"; t="PP_TARGET_$i"
  MUTANT=${!m}; TARGET=${!t}
  mount --bind "$MUTANT" "$TARGET"
  cmp -s "$MUTANT" "$TARGET" || { printf 'probe-pin: MOUNT NOT REACHED: %s (mutant %s)\n' "$TARGET" "$MUTANT" >&2; trap - EXIT; exit 97; }
  i=$((i + 1))
done
exec timeout -k 5 "$TIMEOUT" "$TESTBIN" "$@"
"#;

/// Streams are never merged: stdout must stay a pure libtest JSON record stream, and a stack
/// overflow's `fatal runtime error: …` arrives on stderr.
#[derive(Debug, Clone)]
pub struct Run {
    pub rc: i32,
    pub stdout: String,
    pub stderr: String,
}

/// A signal-killed process has no exit CODE at all — `timeout` re-raises the child's signal
/// rather than translating it. Every shell reports `128 + signal` for that, and so does
/// probe-pin: a stack-overflow abort is the 134 that `HarnessIncomplete`'s message quotes.
#[cfg(unix)]
pub fn exit_rc(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// The non-Unix arm exists so the crate COMPILES off Unix — `cargo check --workspace` on a
/// Windows checkout — not so it runs there: `child_command` shells out to `unshare` and the
/// SCRIPT calls `mount --bind`, so a real isolated run stays Unix-only either way.
///
/// There is no signal to fold in. A terminated process on Windows reports an exit code like
/// any other — the OS carries the termination reason IN the code (`STATUS_STACK_OVERFLOW`
/// surfaces as `0xC00000FD`, not as a separate signal number) — so `128 + signal` has no
/// analogue and `code()` alone IS the exit code. The `unwrap_or` is unreachable on Windows,
/// where `ExitStatus::code()` is always `Some`; 128 is what the Unix arm yields for the
/// matching "no code, no signal" status, so both arms agree on the one status neither
/// platform can describe.
#[cfg(not(unix))]
pub fn exit_rc(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(128)
}

/// The flags probe-pin itself puts on the target's argv — the OWNERSHIP allowlist. Everything
/// else on that command line came from `Target::manifest_argv`, which is what
/// `manifest::validate` checks.
///
/// Token 0 is the one CONDITIONAL flag (`--exact`, iff `filter_match = "exact"`); the rest are
/// unconditional and `argv` iterates `UNCONDITIONAL_FLAGS` rather than respelling them, so the
/// allowlist and the argv it authorizes cannot drift apart.
pub const OWNED_FLAGS: &[&str] = &[
    "--exact",
    "--test-threads=1",
    "--format",
    "json",
    "-Z",
    "unstable-options",
];

/// `OWNED_FLAGS` minus the conditional `--exact`, in argv order — derived, never restated.
pub const UNCONDITIONAL_FLAGS: &[&str] = OWNED_FLAGS.split_at(1).1;

/// `[filter] + ["--exact"] iff Exact + [the flags probe-pin owns] + [target].args`.
///
/// The manifest's contribution is taken from `Target::manifest_argv` rather than read out of
/// the fields again: that function is the single authority for WHICH manifest values reach a
/// target's command line, and validation checks the same list. Reading `target.filter` here
/// directly is exactly how `filter` came to be argv token 0 with nothing validating it.
pub fn argv(target: &Target) -> Vec<String> {
    let contributed = target.manifest_argv();
    let ((_, filter), args) = contributed
        .split_first()
        .expect("manifest_argv yields the filter first, then [target].args");
    let mut v = vec![(*filter).to_string()];
    if target.filter_match == FilterMatch::Exact {
        v.push("--exact".to_string());
    }
    v.extend(UNCONDITIONAL_FLAGS.iter().map(|f| (*f).to_string()));
    v.extend(args.iter().map(|(_, a)| (*a).to_string()));
    v
}

/// The ambient keys the target child keeps. Everything else is cleared.
const INHERITED: &[&str] = &["PATH", "HOME"];

/// The variables `run_with_script` hands the SCRIPT, which reads them by these exact names.
/// The mutant/target pair is one variable per mount, so those two are prefixes.
const SCRIPT_VARS: &[&str] = &["PP_MOUNTS", "TIMEOUT", "TESTBIN"];
const SCRIPT_VAR_PREFIXES: &[&str] = &["PP_MUTANT_", "PP_TARGET_"];

/// Does probe-pin itself set this environment variable in the target child? The single
/// authority for "[target].env may not override this", derived from the two places that set
/// one — and pinned by `env_keys_probe_pin_sets_are_all_refused`, which builds a real child
/// command through the shipping path and asserts every key it sets is refused here.
///
/// `PATH` is the severe one: the child is `unshare … bash -c SCRIPT`, so `PATH` resolves
/// `mount`, `cmp` and `timeout` INSIDE the script — a manifest that sets it to a directory of
/// stubs renders `mount reached: 1 file(s)` for a probe whose mutant was never mounted and
/// never compared (measured: exit 0, green).
///
/// `RUST_MIN_STACK` is deliberately absent: `apply_child_env` sets it as a DEFAULT that
/// `[target].env` is documented and tested to raise, and lowering it cannot forge a green row —
/// the target dies mid-run and `HarnessIncomplete` fires.
pub fn is_probe_pin_owned_env(key: &str) -> bool {
    INHERITED.contains(&key)
        || SCRIPT_VARS.contains(&key)
        || SCRIPT_VAR_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// The step-8 target run's environment is CONSTRUCTED, not inherited: two developers with
/// different shells get the same capture bytes. Scope is this run only — the cargo, rustc and
/// ast-grep shell-outs inherit, because clearing them strips `CARGO_HOME`/`CARGO_TARGET_DIR`.
///
/// The ALLOWLIST is the single authority: `RUST_BACKTRACE` is not re-set to `"0"` here, because
/// an unconditional set makes the clear untestable — it produces identical capture bytes whether
/// or not `env_clear()` ran, which is exactly how row 12's backtrace arm went vacuous. Absent
/// from a cleared environment, libtest reads it as off; `[target].env` can still turn it on.
pub fn apply_child_env(cmd: &mut Command, target: &Target) {
    cmd.env_clear();
    for key in INHERITED {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    cmd.env("RUST_MIN_STACK", "16777216");
    for (k, v) in &target.env {
        cmd.env(k, v);
    }
}

pub fn run(testbin: &Path, mounts: &[Mount], target: &Target) -> Result<Run, Abort> {
    run_with_script(SCRIPT, testbin, mounts, target)
}

/// The target child, fully constructed and not yet spawned. Split out from `run_with_script`
/// so a test can read back — through `Command::get_envs` — exactly which variables probe-pin
/// sets, and prove `is_probe_pin_owned_env` refuses every one of them. A hand-maintained list
/// checked against a hand-maintained list proves nothing; this one is checked against the
/// command that actually runs.
///
/// The probe-pin-owned variables are applied AFTER `apply_child_env`, so `[target].env` cannot
/// win by ordering either — belt and braces, since validation now refuses them by name.
pub fn child_command(script: &str, testbin: &Path, mounts: &[Mount], target: &Target) -> Command {
    let mut cmd = Command::new("unshare");
    cmd.args(["--map-root-user", "--mount", "bash", "-c"])
        .arg(script)
        .arg("probe-pin")
        .args(argv(target));
    apply_child_env(&mut cmd, target);
    cmd.env("PP_MOUNTS", mounts.len().to_string());
    for (i, m) in mounts.iter().enumerate() {
        cmd.env(format!("PP_MUTANT_{i}"), &m.mutant);
        cmd.env(format!("PP_TARGET_{i}"), &m.target);
    }
    cmd.env("TIMEOUT", target.timeout_secs.to_string());
    cmd.env("TESTBIN", testbin);
    cmd
}

/// Split out so §10 rows 1, 2 and 21 can run their DROP arms — each of which is defined as a
/// mutation OF THIS SCRIPT (drop the readback / compare the mutant with itself / copy instead
/// of mount) rather than a mutation of the caller.
pub fn run_with_script(
    script: &str,
    testbin: &Path,
    mounts: &[Mount],
    target: &Target,
) -> Result<Run, Abort> {
    let out = child_command(script, testbin, mounts, target)
        .output()
        .map_err(|e| Abort::IsolationUnavailable {
            rc: None,
            stderr: e.to_string(),
        })?;
    Ok(Run {
        rc: exit_rc(&out.status),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// §7 step 5. `TempDir` in `base` (`env::temp_dir()` in production), then the assert that
/// probe-pin is not creating files under the tree it is measuring.
pub fn scratch_dir(base: &Path, workspace: &Path) -> Result<TempDir, Abort> {
    let dir = TempDir::new_in(base).map_err(|e| Abort::MaterializeFailed {
        path: base.to_path_buf(),
        err: e.to_string(),
    })?;
    let real = std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
    let ws = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    if real.starts_with(&ws) {
        return Err(Abort::ScratchInsideWorkspace {
            scratch: real,
            workspace: ws,
        });
    }
    Ok(dir)
}

/// §7 steps 6 and 10: sha256 over the mutated file set only — scoped to the files probe-pin
/// touches, so a concurrent agent editing anything else cannot discard this run.
pub fn fingerprint(root: &Path, files: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for rel in files {
        let bytes = std::fs::read(root.join(rel))?;
        out.insert(rel.clone(), sha256_hex(&bytes));
    }
    Ok(out)
}

pub fn verify_fingerprint(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Result<(), Abort> {
    for (file, sha) in before {
        let now = after.get(file).map(String::as_str).unwrap_or("<missing>");
        if now != sha {
            return Err(Abort::TreeMutated {
                file: PathBuf::from(file),
                before: sha.clone(),
                after: now.to_string(),
            });
        }
    }
    Ok(())
}
