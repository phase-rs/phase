# probe-pin

probe-pin turns a *probe* — a mutation of the tree plus an expectation about a test's outcome —
into a regenerable, digest-stamped block of comments in source. The block is the artifact a
reviewer reads; the manifest is the thing that regenerates it; the digest is what makes a stale
block loud instead of decorative.

The tool exists to enforce exactly two invariants:

1. **probe-pin never writes the tree it measures.** Mutants are materialized in a scratch
   directory outside the workspace and bind-mounted over the real path inside an unprivileged
   mount namespace. Every mutated file is sha256'd before and after the run. That scratch
   directory is asserted to be outside the workspace — and, since a mutant lands at
   `<scratch>/<probe.id>/<file>`, the two joined KEYS are validated too: an id must be a plain
   name (`[A-Za-z0-9_-]`), and every manifest path must be workspace-relative with no `..`
   component. A container assert cannot see a key that joins its way back out of the container.
   Lexical cleanliness is only the pre-filter: every manifest path is then **canonicalized and
   asserted to resolve inside its base** — the workspace root for a file probe-pin reads, the
   scratch dir for a mutant it writes — because `Path::components()` all `Normal` says nothing
   about what a component resolves to, and an in-tree symlink satisfies it while landing the
   write anywhere on the filesystem.
2. **No verdict is reported for a run that measured nothing.** The floor is on what
   **executed**, not on what was selected: `passed + failed == 0` aborts. `test_count` reads 1
   for a normal run, for an `#[ignore]`d pinned test and for a `--bench` argv alike (measured,
   all three), so a floor on selection is blind to every flag or attribute that suppresses
   execution. One abort covers both shapes and names which it saw — the filter selected
   nothing, or everything it selected was skipped.

Everything else in this document is a bound on what a row in the block actually proves.

## Usage

```bash
cargo probe-pin run          <manifest>   # run every probe, print the block
cargo probe-pin run --write  <manifest>   # ... and splice it into [output].file
cargo probe-pin check        <manifest>   # ... and compare it against the committed block
```

`<manifest>` must live **inside the workspace**: its path is stamped into the BEGIN line and
into the digest, so an absolute one would pin a block no other checkout can reproduce.

`check` costs the **same target runs as `run`** — it is a re-measurement, not a text
comparison. Exit codes:

| exit | meaning |
|---|---|
| 0 | ok / the committed block matches |
| 1 | a verdict mismatched its expectation, or the block is stale |
| 2 | any abort — an instrument failure. Nothing is written. |
| 101 | probe-pin itself panicked or failed to build (out of contract) |

The local enforcement venue is the Tilt `probe-pin-check` resource. **`probe-pin check` is not
enforced in GitHub CI**: enrolling it needs a `.github/workflows/**` edit, which is a hard stop
for agent changes. CI does build the crate and run its tests (`--workspace`).

**The Tier-2 suite does not run in CI, and this is measured, not assumed.** GitHub's runners deny
unprivileged user namespaces — `unshare --map-root-user --mount` fails with
`write failed /proc/self/uid_map: Operation not permitted` — so every test in
`tests/isolation_e2e.rs` carries `#[ignore]`. CI therefore covers `pure_logic` (schema,
validation, verdict, digest and drift logic) but never exercises a real mount. Run Tier 2 where
namespaces work:

```bash
cargo test -p probe-pin --test isolation_e2e -- --ignored
```

`#[ignore]` rather than a runtime capability check, on purpose: a test that silently no-ops when
the namespace is unavailable reports as **passed**, which is exactly the unmeasured-green this
tool exists to refuse. An ignored test reports as ignored.

## Manifest

```toml
version = 1                                  # unknown value -> refuse

[target]
mode         = "runtime-read"                # "compiled" parses and is REJECTED (see below)
package      = "phase-engine"                # cargo -p     — binary RESOLUTION only.
                                             # The PACKAGE name, which is not the lib TARGET name:
                                             # crates/engine is package `phase-engine`, [lib] `engine`,
                                             # and `-p engine` fails to resolve.
test         = "integration"                 # cargo --test
filter       = "loop_shortcut_seat_pin_census"   # a SUBSTRING: may not begin with '-'
filter_match = "substring"                   # or "exact" -> probe-pin appends --exact
args         = []                            # appended LAST to the argv; reserved flags refused
env          = { RUST_MIN_STACK = "16777216" }   # DEFAULT, not empty; probe-pin's own keys refused
timeout_secs = 300                           # exceeded -> killed, and named; 0 is REFUSED

[output]
file   = "crates/engine/tests/integration/loop_shortcut_seat_pin_census.rs"
marker = "PROBE-PIN"                         # [A-Za-z0-9_-] too: the marker IS the block's
                                             # structure, and a blank one degenerates to
                                             # ':BEGIN'/':END'

[[probe]]
id    = "P0_control"                         # [A-Za-z0-9_-]: an id is a scratch path SEGMENT
claim = "no mounts, unmodified tree — the instrument itself"
  [probe.expect]
  outcome = "pass"
# EXACTLY ONE zero-mutation probe is required: it is the control.

[[probe]]
id = "P1_revert_interaction"
  [[probe.mutation]]                         # 0..N, APPLIED IN ORDER
  kind = "replace"                           # or "prepend" { files, text, repeat }
                                             # every mutation must name >= 1 file, and a pad
                                             # (text.len() × repeat) is capped at 64 MiB
  file = "crates/engine/src/game/interaction.rs"
  find = "…verbatim…"                        # must match EXACTLY once, in the RUNNING text
  replace = ""                               # may be ""; the MATERIALIZED file must differ
  [[probe.assert_count]]                     # checked on the FINAL mutant text, so the file
                                             # must be one THIS probe mutates
  file = "crates/engine/src/game/interaction.rs"
  text = "TargetPin::Player"
  count = 1
  [probe.expect]
  outcome = "fail"
  anchor  = ["PROVENANCE SPLIT VIOLATED", "text: \"Ok(TargetPin::Player(*player))\""]

[[projection]]                               # 0..N, optional; shells to ast-grep
id       = "targetpin_sites"                 # a plain name, and UNIQUE: counts are looked up by
                                             # id, so a duplicate renders the FIRST one's number
pattern  = "TargetPin::Player($$$)"
paths    = ["crates/engine/src", "crates/server-core/src"]   # >= 1: ast-grep with no operand
                                             # scans the whole cwd instead
sentence = "`TargetPin::Player` is constructed or matched at {count} sites."   # {count} REQUIRED
```

probe-pin owns `--format`, `-Z`, `--nocapture`, `--show-output`, `--test-threads`, `--quiet` and
`--exact`; no manifest value may pass them. It runs the target with libtest's JSON record
format (`--format json -Z unstable-options`, **nightly-only surface**) and requires a pure
record stream on stdout, so `--nocapture` would corrupt the instrument.

That check is applied to **every token the manifest contributes to the argv**, enumerated once
in `Target::manifest_argv` and used by `isolate::argv` to build the real argv — `filter` reaches
libtest too, as token 0, where getopts parses a `-`-leading token as a flag. `filter` is
additionally constrained by shape: it is a substring matched against test names, so it may not
begin with `-`. Hostile flags are *not* enumerated (`--bench` is on no list); the execution
floor is what covers them, this is what keeps the two argv producers from drifting.

`[target].env` may not set a variable probe-pin itself sets — `PATH`, `HOME`, `PP_MOUNTS`,
`PP_MUTANT_*`, `PP_TARGET_*`, `TIMEOUT`, `TESTBIN`. The target child is `unshare … bash -c
SCRIPT`, so `PATH` resolves the `mount` and `cmp` the readback is made of: a manifest pointing it
at stubs renders `mount reached: N file(s)` for a probe whose mutant was never mounted (measured,
exit 0). `RUST_MIN_STACK` is the one probe-pin-set variable a manifest may override — it is a
default, and lowering it kills the target mid-run rather than forging a green row.

Anchors match **inside one `{"type":"test"}` `event:"failed"` record's capture buffer** — that
test's own `println!`, `eprintln!` and panic message, and nothing from any other test. All
anchors in the list must hit inside the same record. An anchor that is empty after trimming is
refused for the same reason the empty anchor LIST is: `contains("")` is true of every capture,
so it is satisfied by any failure at all.

**Every manifest value that is RENDERED into the block is refused if it carries a control
character or a marker tag.** The block is line-oriented and marker-delimited — `block::locate`
counts `<marker>:BEGIN`/`<marker>:END` per line, and the cell escape covers only `|` — so such a
value does not describe the artifact, it reshapes it. Measured: `anchor = ["PROBE-PIN:END"]`
rendered into the firing-assertion cell, `run --write` exited 0, and the spliced file then held
two END markers, after which every `check` aborts `expected exactly one PROBE-PIN:BEGIN/END pair
… found 1/2` and the pin can only be recovered by hand. The rule is applied to the SURFACE, not
to `anchor`: `manifest::rendered_block_values` enumerates every value that reaches the block —
the manifest path, `[output].marker`, each probe id, each mutation path (its file name is
rendered in the mutation cell), each anchor, each projection sentence — and
`pure_logic::block_text_surface_is_one_authority` measures that enumeration against what
`block::render` actually emits, so a new field that starts rendering is either covered or the
census fails.

**Named residual.** The same census also names the two producers on this surface that are *not*
manifest values and cannot be refused before the run: an instrument's `--version` string, and a
`provenance` path taken from the target's own `panicked at …` output. Reaching the failure
through those needs a tool version or a source file whose name contains `<marker>:END`, and the
result is loud (`check` aborts `MarkerNotUnique`) rather than a green row for an unmeasured
tree — so they are disclosed, not guarded.

## What a row proves, and what it does not

**`fail` rows.** `Verdict::Fail` means the target failed *and* every anchor hit inside one
failed-test record. A target failure whose anchors do not all land in one record is reported as
a verdict mismatch (exit 1) naming the missed anchor — never as a pass — and `run --write` does
not splice a block for a run that exits non-zero.

**`pass` rows** prove **visible-and-still-passing**, not read-and-still-passing. The mutant
differed from the original, was visible at the target's path inside the namespace (proved by a
`cmp` readback), the harness ran to completion, and the assertion still passed. The row's
"firing assertion" cell renders `mount reached: N file(s)` to say exactly that. Three ways a
`pass` row is silent about reading, all measured:

1. **`include_str!` bakes the original bytes at build time.** The target binary is built
   outside the namespace, by design, so a compile-time read can never see the mutant.
2. **A bind mount is path-scoped.** Any second hardlink to the same inode still serves the
   original bytes.
3. A manifest naming a file the target's scan root no longer covers yields a green pass row
   forever.

## Isolation model

Each probe runs as `unshare --map-root-user --mount bash -c <script>`; the script bind-mounts
every mutant over its real path, `cmp -s`-reads each one back, and `exec timeout -k 5 …`s the
target. The mounts are private to that process and vanish on exit.

**The script owns two exit codes, and the classification reads them before the record stream.**
`97` is the mount readback failing; `96` is the script failing before the target was reached at
all — `mount`, `cmp` and `timeout` are resolved through `PATH` *inside* the namespace under
`set -eu`, so a missing one of them, an unset `PP_*` variable, or a failed `exec` kills the shell
with **empty stdout and a non-zero code**, which is byte-for-byte the shape of a target that died
before its first libtest record. Measured with `timeout` off `PATH`: exit 127, and the abort
raised was `HarnessIncomplete`, whose message tells the operator to raise `RUST_MIN_STACK`
through `[target].env` — a remedy that cannot make `timeout` exist. An `EXIT` trap now raises 96
for the whole pre-`exec` region (a successful `exec` replaces the shell, so it cannot fire for
the target's own exit), and both reserved codes are trusted only when stdout is empty.

**Mount-isolated is not write-isolated (N-10).** `unshare --mount` stops *probe-pin* from
writing your tree. It does not stop the *target* from writing anywhere it likes, including
gitignored `target/`. probe-pin's guarantees are about probe-pin, not about your test.

**`env_clear()` is scoped to the target run only (MAJOR-3).** The target child's environment is
constructed — `PATH`, `HOME`, `RUST_MIN_STACK` (default `16777216`, overridable), then
`[target].env` — so two developers with different shells pin the same capture bytes.
`RUST_BACKTRACE` is deliberately **not** re-set: an unconditional set produces identical capture
bytes whether or not the clear ran, which is how row 12's backtrace arm went vacuous. Absent from
a cleared environment libtest reads it as off, and `[target].env` can still turn it on. The `cargo`, `rustc` and `ast-grep` shell-outs **inherit** the ambient environment
unmodified: clearing them would strip `CARGO_HOME`/`CARGO_TARGET_DIR` and silently resolve
against the shared `~/.cargo` instead of a per-worktree home.

**Scratch location.** Mutants go in a `TempDir` under `TMPDIR`, and probe-pin asserts that
directory is not inside the workspace. If your `TMPDIR` is inside the worktree, probe-pin
aborts rather than creating files under the tree it is measuring.

## Anchors may not embed line numbers — and the bound on that lint

A source line number moves on every unrelated edit above it, so an anchor containing one rots
without the pinned behaviour changing. Validation rejects three forms, at zero target runs:

```
\bline\s*[:=]?\s*\d      ∪      \.[A-Za-z]{1,5}:\d+      ∪      :\d+:\d+
```

Measured against the ten shipping anchors of the real corpus (0 rejected) and nine hostile
spellings (7 rejected).

**Named residual.** The lint cannot catch a positional integer that is syntactically
indistinguishable from a legitimate count. The boundary is two strings that differ only in the
integer:

```
shipping anchor : ("engine/src/game/engine.rs", 1)       accepted (correctly)
hostile twin    : ("engine/src/game/engine.rs", 11492)   accepted (the miss)
```

`L11492` is the other measured miss. Any regex that rejects the hostile form also rejects the
real shipping anchor, so no lint closes this. The behavioural alternative — re-running each
probe against a line-shifted tree — is measurably unsound (a line number originating in a file
no probe mutates survives it) and costs a target run per probe, so it is not shipped. Revisit if
an anchor of the `("<path>", <int>)` shape is ever authored with a *line* in the integer slot.

Two probes whose anchor lists are byte-identical, or whose anchor list fully matches another
probe's captured failure, are refused: they do not distinguish each other, so the pin would stay
green if one of the two mutations stopped firing. The escape is to merge the two probes or to
make the target's assertion message distinguish them — *not* to add a unique anchor, which may
be impossible when the only distinguishing text is a line number.

## The block, the digest, and instruments

```
// human intent prose lives HERE, above the marker, and is never touched — and never validated
// PROBE-PIN:BEGIN manifest=probe-pin/seat-pin.toml digest=sha256:eb93eb0d0322246d
// instrument rustc = rustc 1.97.0-nightly (0febdbab2 2026-04-18)
// instrument ast-grep = ast-grep 0.44.1
// | probe | mutation | expect | verdict | firing assertion (anchor) | provenance |
// |---|---|---|---|---|---|
// | P0_control | (none) | pass | pass | (control; no mounts) | — |
// probe-pin validates only the lines between BEGIN and END. Prose outside is never checked.
// PROBE-PIN:END
// more human prose HERE, below the marker, also never touched and also never validated
```

**Out-of-marker prose is a known non-guarantee.** probe-pin validates the lines between `BEGIN`
and `END`. Prose above, below, or anywhere else is never read, never validated, and never
invalidated by this tool. Detecting stale free prose requires reading intent, and no instrument
does that. If you write interpretation, label it yourself.

**The digest pins what was MEASURED**, not just the outcomes: the whole `[target]` (including
`filter`, `filter_match`, `args`, `env` and `timeout_secs`), `[output]`, every probe's inputs and
its observed exit code and verdict, the control, every projection's `pattern` and sorted `paths`
and count, and the instrument list. Narrowing a filter or subsetting a projection's paths
changes the digest even when every verdict and count is identical. Excluded: `:line:col`,
durations, PIDs, absolute paths, and completion order.

**Line numbers are excluded from the render, the digest and anchors** — one rule applied three
times. A line number is not evidence a committed artifact can carry, because it changes without
the pinned behaviour changing. If you need the line, run the target: probe-pin's messages name
the file and the assertion text.

**An instrument is recorded iff it produced a number in the block.** `rustc` always did — every
verdict came through libtest's nightly-only JSON surface — so it is unconditional. `ast-grep`
only did when the manifest declares at least one projection, so its line appears only then.
`PROBE_PIN_ASTGREP` overrides the ast-grep binary; there is no `rustc` twin, because `rustc` is
on `PATH` wherever cargo built the target.

**Projections are per-manifest.** A manifest that declares one and cannot run the tool aborts:
probe-pin will not emit a block whose projection sentence is silently missing.

**`run --write` refuses** when an instrument moved **and** a measured number moved — exactly
when attribution is impossible and obeying would destroy the evidence of which one changed it.
The escape needs no flag: reduce the block to a bare `BEGIN`/`END` pair and re-run, which shows
the deliberate act in the PR diff. A toolchain bump alone (measured numbers identical) simply
re-stamps; `rust-toolchain.toml` has moved twice in this repository's entire history.

## Compiled mode (deferred)

`[target].mode = "compiled"` parses and is rejected at validation time with an explicit
deferral message. Only `runtime-read` ships: the mutant is bind-mounted and read as **text**,
never compiled.

It is deferred rather than removed because the guards it needs are known and measured. Cargo
fingerprints on mtime; a bind mount presents the mutant's mtime and unmounting restores an
*older* one, so cargo skips the rebuild and the next verdict is silently the previous probe's:

```
### mutant, fresh mtime ###        Compiling probe-scratch = 1  -> test result: FAILED  (correct)
### control immediately after ###  Compiling probe-scratch = 0  -> test result: FAILED  (WRONG)
### control on a FRESH target dir ###                           -> test result: ok. 1 passed
```

`touch`ing the real source fixes it and writes your worktree, which is prohibited. Compiled mode
therefore needs **both**: a fresh `CARGO_TARGET_DIR` per probe under the tool's scratch root, and
a mandatory `Compiling <package>` assertion on every compiled build, the control included —
absence aborts. Until that ships, prose about compiled behaviour has no instrument here, and its
honest home is outside the markers, where probe-pin explicitly never validates it.
