# phase.rs — local development orchestration
#
# Usage:
#   tilt up                              core dev loop (wasm + frontend + lobby worker)
#   tilt up -- server                    also start the game server
#   tilt up -- test lint                 also start test runners and linters
#   tilt up -- server test lint          full stack
#   tilt up -- tauri                     desktop app (replaces frontend)
#
# All resources are always visible in the Tilt UI — opt-in groups just
# control which auto-start. Click any stopped resource to start it on demand.

config.define_string_list('enable', args = True, usage = 'Resource groups to auto-start: server, tauri, test, lint, https')
enabled = config.parse().get('enable', [])

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

# Editor/agent tools write `<file>.tmp.<pid>.<hash>` staging files next to the
# real file before renaming into place; without this, every such temp file
# restarts the watching resources mid-build.
TMP_IGNORE = ['**/*.tmp.*']

# probe-pin isolates through `unshare --map-root-user --mount` (util-linux) and runs its target
# under `timeout` (GNU coreutils). macOS ships neither, and there is no Darwin equivalent of an
# unprivileged mount namespace to port to — so EVERY probe-pin resource in this file aborts on
# a Darwin host no matter what the tree contains: `probe-pin check` reaches `isolate::run`
# unconditionally from `pipeline()` (crates/probe-pin/src/main.rs:145 — the baseline run, before
# any mutant), and that spawns `unshare` (crates/probe-pin/src/isolate.rs:157). Scoped as a rule
# over all of them rather than as a count: a count goes stale the next time one is added, which
# is precisely how a resource below came to sit here ungated. Gate their auto_init rather than
# let them boot straight into a permanent red: a gate that is red on every change teaches
# everyone to stop reading the colour, which costs more than the gate earns. They stay VISIBLE
# and clickable, like every other opt-in resource here, so the refusal is still one click away
# when someone wants to see it.
# `os.name` is consulted FIRST and short-circuits, so a Windows host never reaches `uname`
# — which it does not ship, and which would fail Tiltfile LOAD rather than one resource.
IS_LINUX = (
    os.name == 'posix'
    and str(local('uname -s', quiet = True, echo_off = True)).strip() == 'Linux'
)

# auto_init alone would NOT be enough: it governs only the STARTUP run, and the default
# TRIGGER_MODE_AUTO re-runs a resource whenever its deps change. Every probe-pin resource lists
# 'crates/probe-pin/' in `deps`, so off Linux the very next edit there would drag it back into
# the red that auto_init just avoided. Off-Linux they must stop watching too, not merely stop
# booting — so the two gates travel together: a probe-pin resource carrying only one of them is
# a bug, not a lighter reading of the policy.
PROBE_PIN_TRIGGER = TRIGGER_MODE_AUTO if IS_LINUX else TRIGGER_MODE_MANUAL

# Must stay a SUPERSET of what `scripts/engine-source-hash.sh` hashes as the engine cache
# key (src + data + build.rs + Cargo.toml). `data/` is `include_str!`d into the binary and
# `build.rs`/`Cargo.toml` change what gets compiled, so a change to any of them changes the
# engine -- but a resource only rebuilds on its `deps`, and `tilt-wait.sh` derives build
# freshness from those same `deps`. So anything hashed-but-unwatched is doubly invisible:
# Tilt does not rebuild, AND the freshness scan does not look there, so `tilt-wait.sh
# card-data` answers "fresh + ok" for a change that was never compiled. Under-specifying
# `deps` here silently re-opens the false green that tilt-wait.sh exists to close.
ENGINE_SRC = [
    'crates/engine/src/',
    'crates/engine/data/',
    'crates/engine/build.rs',
    'crates/engine/Cargo.toml',
]
ENGINE_TESTS = ['crates/engine/tests/']
AI_SRC = ['crates/phase-ai/src/']
AI_TESTS = ['crates/phase-ai/tests/']
WASM_SRC = ['crates/engine-wasm/src/']
DRAFT_CORE_SRC = ['crates/draft-core/src/']
DRAFT_WASM_SRC = ['crates/draft-wasm/src/']

# The wasm32 build gets its own target root too. Although it writes to a
# distinct target/wasm32-unknown-unknown/ subdir, the cargo build lock is per
# target ROOT, so without this it would still serialize behind native builds.
# Its own root lets it compile in parallel — hence no resource_deps on clippy.
# build-wasm.sh honors CARGO_TARGET_DIR (defaulting to target/ for CI/deploy
# callers that don't set it), so only this dev-loop invocation is relocated.
local_resource('wasm',
    # List-form cmd: Tilt runs a STRING cmd through the platform shell (cmd.exe on
    # Windows), which can't parse POSIX `VAR=val cmd` syntax or `./script.sh` --
    # and worse, cmd.exe mangles nested double-quotes before bash ever sees them,
    # so a 'bash -c "..."' STRING wrapper fails with "unexpected EOF" (found the
    # hard way). A list-form cmd bypasses cmd.exe's string parsing entirely --
    # Tilt execs the argv array directly, so bash receives the script text as one
    # untouched element. On Mac/Linux this is equally valid and equally a no-op.
    cmd = ['bash', '-c', 'CARGO_TARGET_DIR=target/wasm ./scripts/build-wasm.sh'],
    deps = ENGINE_SRC + AI_SRC + WASM_SRC + DRAFT_CORE_SRC + DRAFT_WASM_SRC,
    ignore = TMP_IGNORE,
    allow_parallel = True,
    labels = ['build'],
)

# ---------------------------------------------------------------------------
# Serve
# ---------------------------------------------------------------------------

# When the Caddy HTTPS proxy is in the loop, set CADDY_PROXY=1 so vite.config.ts
# rewrites the injected HMR client to talk wss://local.phase-rs.dev:443 instead
# of ws://localhost:5173 — the page is served from the proxy origin, so the
# default would silently fail the mixed-origin / mixed-content checks.
local_resource('frontend',
    serve_cmd = 'pnpm dev',
    serve_dir = 'client',
    serve_env = {'CADDY_PROXY': '1'} if 'https' in enabled else {},
    auto_init = 'tauri' not in enabled,
    allow_parallel = True,
    links = ['http://localhost:5173'],
    labels = ['serve'],
)

# Deck-import + lobby broker Worker. vite.config.ts proxies /import-deck to
# :8787 unconditionally, so without this process the "Import from URL" deck
# flow fails with a Vite-generated 500: a connection refusal wearing the
# costume of a server bug, with nothing naming the missing service. It
# therefore starts with the core loop rather than sitting behind an opt-in
# group, because the feature it backs ships in the default frontend.
#
# No `deps`, for the same reason `frontend` above carries none: wrangler
# watches lobby-worker/src/ and reloads itself, so listing deps here would
# restart the process out from under its own hot reload. wrangler.toml's
# [build] runs scripts/build-broker-wasm.sh, which compiles
# lobby-worker/broker-wasm/ into that crate's own target dir, so it never takes
# the workspace cargo lock.
local_resource('lobby-worker',
    serve_cmd = 'npm run dev',
    serve_dir = 'lobby-worker',
    allow_parallel = True,
    links = ['http://localhost:8787'],
    labels = ['serve'],
)

# HTTPS reverse proxy for LAN testing — required so WebRTC (PeerJS P2P
# hosting) and crypto.randomUUID work for guest devices, which both refuse
# to operate on insecure origins other than localhost. Bound to :443 via
# the macOS 0.0.0.0 quirk (see scripts/run-caddy.sh) so no sudo is needed.
# Run `./scripts/setup-ssl.sh` once before first use.
local_resource('caddy',
    serve_cmd = './scripts/run-caddy.sh',
    deps = ['Caddyfile', 'certs/local.phase-rs.dev/server.crt'],
    resource_deps = ['frontend'],
    auto_init = 'https' in enabled,
    allow_parallel = True,
    links = ['https://local.phase-rs.dev'],
    labels = ['serve'],
)

# Thin-shell dev loop. `tauri dev` starts vite itself (beforeDevCommand) and
# points the window at devUrl http://localhost:5173, so the shell hosts the
# LOCAL frontend instead of the production bootstrap->remote-origin flow —
# that is why `frontend` sets auto_init = 'tauri' not in enabled (both would
# bind :5173). The shell crate is workspace-excluded and self-contained, and
# `tauri dev` watches client/src-tauri/src/ and rebuilds on its own; Tilt only
# restarts the loop when the Tauri config or crate manifest changes. The old
# phase-server sidecar build is gone with the thin shell: production shells
# download their native engine via signed manifests, and local multiplayer
# testing talks to the `server` resource on :9374.
local_resource('tauri',
    serve_cmd = 'pnpm tauri:dev',
    serve_dir = 'client',
    deps = ['client/src-tauri/tauri.conf.json', 'client/src-tauri/Cargo.toml'],
    ignore = TMP_IGNORE,
    auto_init = 'tauri' in enabled,
    labels = ['serve'],
)

SERVER_SRC = ENGINE_SRC + AI_SRC + [
    'crates/server-core/src/',
    'crates/phase-server/src/',
]

local_resource('server',
    cmd = 'cargo build -p phase-server --bin phase-server',
    serve_cmd = './target/debug/phase-server',
    serve_env = {'PHASE_DATA_DIR': 'data'},
    deps = SERVER_SRC,
    ignore = TMP_IGNORE,
    auto_init = 'server' in enabled,
    links = ['http://localhost:9374'],
    labels = ['serve'],
)

# ---------------------------------------------------------------------------
# Test
# ---------------------------------------------------------------------------

# Compile the native test harnesses once, then let the test runners fan out to
# parallel execution. Without this, test-engine and test-ai each serialize on
# the cargo build lock during their compile phase. `--no-run` builds the test
# binaries without executing them; the downstream `cargo nextest run -p ...` then
# finds everything fingerprint-fresh and just runs (no recompile). nextest (the
# same runner CI uses) schedules every test across all binaries in one global
# pool, overlapping the lib and integration harnesses instead of running them
# back-to-back like `cargo test` — much faster local feedback at zero compile
# cost. Default features — matching the test resources, which (unlike
# `cargo test-all`) do not enable engine/proptest; a feature mismatch here would
# force a rebuild.
local_resource('build-native',
    cmd = 'cargo nextest run -p phase-engine -p phase-ai --no-run',
    deps = ENGINE_SRC + ENGINE_TESTS + AI_SRC + AI_TESTS,
    ignore = TMP_IGNORE,
    allow_parallel = True,
    auto_init = 'test' in enabled,
    labels = ['test'],
)

local_resource('test-engine',
    cmd = 'cargo nextest run -p phase-engine',
    deps = ENGINE_SRC + ENGINE_TESTS,
    ignore = TMP_IGNORE,
    resource_deps = ['build-native'],
    allow_parallel = True,
    auto_init = 'test' in enabled,
    labels = ['test'],
)

local_resource('test-ai',
    cmd = 'cargo nextest run -p phase-ai',
    deps = ENGINE_SRC + AI_SRC + AI_TESTS,
    ignore = TMP_IGNORE,
    resource_deps = ['build-native'],
    allow_parallel = True,
    auto_init = 'test' in enabled,
    labels = ['test'],
)

local_resource('test-frontend',
    cmd = 'pnpm test -- --run',
    dir = 'client',
    deps = ['client/src/'],
    ignore = TMP_IGNORE,
    resource_deps = ['wasm'],
    allow_parallel = True,
    auto_init = 'test' in enabled,
    labels = ['test'],
)

# ---------------------------------------------------------------------------
# Lint
# ---------------------------------------------------------------------------

# clippy builds into its own target root. The clippy driver writes different
# fingerprints than `cargo build`/`cargo test` into the shared target/debug,
# mutually invalidating artifacts (rebuild thrash). A separate CARGO_TARGET_DIR
# also gives it its own build lock, so it never queues behind the native test
# builds. Cost: a second debug tree on disk (reclaimed by cargo-sweep).
local_resource('clippy',
    # List-form cmd: see the 'wasm' resource above for why (a STRING 'bash -c "..."'
    # gets its quotes mangled by cmd.exe on Windows; list-form bypasses that).
    cmd = ['bash', '-c', 'CARGO_TARGET_DIR=target/clippy cargo clippy --all-targets -- -D warnings && CARGO_TARGET_DIR=target/clippy ./scripts/check-interaction-bindings.sh --check'],
    deps = ['crates/', 'client/src/adapter/generated/interaction/index.ts', 'scripts/check-interaction-bindings.sh'],
    ignore = TMP_IGNORE,
    auto_init = 'lint' in enabled,
    allow_parallel = True,
    labels = ['lint'],
)

local_resource('check-frontend',
    cmd = 'pnpm run type-check && pnpm lint',
    dir = 'client',
    deps = ['client/src/'],
    ignore = TMP_IGNORE,
    allow_parallel = True,
    auto_init = 'lint' in enabled,
    labels = ['lint'],
)

# ---------------------------------------------------------------------------
# Data (manual trigger — click in UI to run)
# ---------------------------------------------------------------------------

local_resource('card-data',
    # List-form cmd: see the 'wasm' resource above for why (a STRING 'bash -c "..."'
    # gets its quotes mangled by cmd.exe on Windows; list-form bypasses that).
    cmd = ['bash', '-c', './scripts/gen-card-data.sh'],
    deps = ENGINE_SRC,
    # gen-card-data.sh promotes these tracked files under crates/engine/data/, which is
    # in ENGINE_SRC (deps). Watching card-data's own generated outputs makes every
    # promote re-trigger card-data -> an infinite regen loop. The script already stages
    # via a `.tmp.` infix (covered by TMP_IGNORE), but the final promote writes the real
    # tracked file, which TMP_IGNORE can't mask. Ignoring the outputs here breaks the
    # self-trigger without touching the engine resources, which still watch
    # crates/engine/data/ in full so a genuine data change still rebuilds the engine.
    ignore = TMP_IGNORE + [
        'crates/engine/data/known-tokens.toml',
        'crates/engine/data/oracle-subtypes.json',
        'crates/engine/data/mtgjson-vintage',
    ],
    auto_init = True,
    labels = ['data'],
)

local_resource('draft-pools',
    cmd = 'cargo run --bin draft-pool-gen',
    deps = DRAFT_CORE_SRC + ['data/mtgjson/sets/'],
    ignore = TMP_IGNORE,
    auto_init = True,
    labels = ['data'],
)

local_resource('coverage',
    cmd = 'cargo coverage',
    resource_deps = ['card-data'],
    trigger_mode = TRIGGER_MODE_MANUAL,
    auto_init = False,
    labels = ['data'],
)

# The only local enforcement venue for `probe-pin check` (CI enrollment needs a workflow edit,
# which is a hard stop). Measured cost: 6.25s cold, 0.11s incremental, 0.046s for 10 isolated
# probe runs -- so an automatic trigger with narrow deps, not a manual knob. This price holds
# ONLY while the manifest pins probe-pin's own test binary. RE-PRICED IN PART for the engine-census
# manifest added below: the estimate was right that an engine build is additional (measured 32.86s
# on an ENGINE_SRC edit, even after build-native) and high on the runs (measured 14.7s, not ~25s).
# The AS-SHIPPED price of 'probe-pin-census''s own cmd is cold 22.99s / incremental 15.61s in an
# isolated tree; the price under a real `tilt up` -- shared target/, build-lock contention -- is
# NOT measured and is owed by whoever next prices this pair.
local_resource('probe-pin-check',
    # Separate CARGO_TARGET_DIR (same reason as 'clippy'): probe-pin's dep tree is disjoint
    # from the engine's, so a shared dir would mutually invalidate fingerprints.
    cmd = ['bash', '-c',
           'CARGO_TARGET_DIR=target/probe-pin cargo probe-pin check crates/probe-pin/tests/fixtures/dogfood.toml'],
    # 'rust-toolchain.toml': the digest covers used_instruments() (unconditionally [Toolchain]), so
    # a rustc move alone is `check` exit 1. The channel is date-pinned, so watching it puts that red
    # on the causing commit. Full reasoning at the 'probe-pin-census' resource (named, not adjacent).
    deps = ['crates/probe-pin/', 'docs/probe-pin.md', 'rust-toolchain.toml'],
    # TMP_IGNORE is a FILENAME glob ('**/*.tmp.*') and does not match a tmp/ DIRECTORY, so the
    # Tier-2 tests' scratch writes under tests/fixtures/tmp/ would retrigger this resource.
    ignore = TMP_IGNORE + ['**/tmp/**'],
    auto_init = 'lint' in enabled and IS_LINUX,
    trigger_mode = PROBE_PIN_TRIGGER,
    allow_parallel = True,
    labels = ['lint'],
)

# The Tier-2 suite is #[ignore]d because GitHub's runners deny unprivileged user namespaces
# (unshare -> /proc/self/uid_map EPERM), so GH CI never executes a real mount. THIS resource is
# what keeps that honest: a LINUX local venue does have the capability, so Tier 2 runs there on
# every change. Without it, "ignored in CI" quietly becomes "never run anywhere".
#
# On a non-Linux host, "never run anywhere" is not a hedge — it is the literal state. CI cannot
# mount, and Darwin has no `unshare` to try, so Tier 2's claims are carried entirely by whatever
# Linux venue last executed them. IS_LINUX only stops this resource auto-starting into a red it
# can never clear; it does not make that gap smaller. The tests are deliberately NOT cfg'd out,
# so a manual run here still prints probe-pin's own named refusal rather than a zero-test green.
#
# `-- --ignored` runs ONLY the ignored tests, which is exactly this suite. A non-zero exit turns
# the resource red like any other gate — a real Tier-2 gate, not a fire-and-forget reporter.
#
# Its OWN CARGO_TARGET_DIR, not 'probe-pin-check''s. Both resources watch 'crates/probe-pin/', so
# one edit triggers both, and both set allow_parallel — so Tilt may run them at once. Sharing a
# target dir across them is not the disjoint-dep-tree case the comment above describes: each
# isolation test shells a nested `cargo test --test pure_logic --no-run` into that dir while
# `probe-pin check` resolves the same test binary out of it. The contention is cross-PROCESS, so
# the in-process SERIAL mutex in isolation_e2e.rs does not span it, and the flake recorded there
# has exactly this mechanism (artifact freshness). This is the only venue that executes Tier 2, so
# a flake here is a gate reporting the wrong colour.
local_resource('probe-pin-e2e',
    cmd = ['bash', '-c',
           'CARGO_TARGET_DIR=target/probe-pin-e2e cargo test -p probe-pin --test isolation_e2e -- --ignored'],
    deps = ['crates/probe-pin/'],
    ignore = TMP_IGNORE + ['**/tmp/**'],
    auto_init = 'lint' in enabled and IS_LINUX,
    trigger_mode = PROBE_PIN_TRIGGER,
    allow_parallel = True,
    labels = ['lint'],
)

# The engine-census pin -- the re-pricing the `probe-pin-check` note above demands, measured in an
# isolated tree with a dedicated CARGO_TARGET_DIR (never shared), against the probe set exactly as
# the manifest authors it. ⚠ NO TREE SHA IS STAMPED HERE, deliberately. The earlier stamp
# (`59cc90a8`) named a tree the manifest DOES NOT EXIST IN -- a coordinate that identifies nothing.
# (An earlier draft also called that object UNREACHABLE. Deleted, because it is false whenever a
# worktree HEAD still points at it -- and "absent from that tree" was always the whole reason.)
# THE OPERATIVE GUARD IS THE PROBE COUNT below:
# it is a property of this manifest, re-checkable from this file, and it is what actually decides
# whether these figures still describe the set. THE ROWS BELOW ARE A CLOSED RECORD OF ONE MEASUREMENT --
# the set held 9 probes when timed -- and NOT a description of whatever the manifest holds when you
# read this: if a probe is added or removed these figures must be RE-TAKEN, not re-labelled.
#
#   tool build, cold (fresh target/probe-pin)    6.91s   tool build, incremental   0.10s
#   the full probe set, engine target unchanged 14.7s    (two runs: 14.69 / 14.80)
#   the full probe set, AFTER an ENGINE_SRC edit 47.1s   SEQUENTIALLY AFTER build-native, and
#                                                        with the shared target/ already warm
#   THIS RESOURCE'S OWN cmd, as shipped:  cold 22.99s   incremental 15.61s   (isolated tree)
#
# EVERY row above, the last included, was timed BEFORE this resource existed -- the earlier wording
# ("every row except the last") implied the last one was taken through the resource, and it was not.
# The rows above it ran through the `cargo probe-pin` alias; the last is the resource's own cmd, run
# by hand, which is the price 'probe-pin-check''s note actually
# asks for; it is taken in an isolated tree whose dedicated CARGO_TARGET_DIR stands in for the
# shared target/, so it does NOT cover the two hazards below -- those need a real `tilt up`.
#
# => resource cold: read the as-shipped row above (22.99s), which measures it directly; the
#    earlier SUM-of-alias-rows derivation is deleted, superseded by that row per this file's rule.
#    ~14.8s when only this manifest or the census file changed;
#    ~47.2s on an engine-source edit, which is the common trigger -- under the two premises
#    named on the 47.1s row above, both of which this resource's own wiring can violate. See
#    "TWO PRICE HAZARDS" below; they are DISCLOSED, not measured.
#
# THE ENGINE BUILD *IS* ADDITIONAL -- 'probe-pin-check''s note was right to flag it, and measuring
# it was not a formality. `build-native` runs `cargo nextest run -p phase-engine -p phase-ai --no-run`;
# probe-pin's inner resolve runs `cargo test -p phase-engine --test integration --no-run`. The
# two keep SEPARATE artifact sets (different feature unification across the -p set), so after an
# ENGINE_SRC touch the inner resolve costs 32.86s even with build-native already complete --
# against 31.20s with no build-native at all. It saves 1.7s of 32.9s. Both sets then sit at a
# fixed point until the next source change: 0.42s / 0.12s respectively, so they do NOT ping-pong.
#
# TWO PRICE HAZARDS THE FIGURES ABOVE DO NOT COVER -- INCLUDING the as-shipped row. Stated, NOT
# measured: both are properties of a real `tilt up` and neither can be timed from a shell, so
# pricing this resource's cmd in an isolated tree does not settle them. BOTH NUMBERS ARE STILL
# OWED (see 'probe-pin-check''s note above).
#  1. `tilt up -- lint` PAYS A PARTIAL COLD ENGINE BUILD. Every figure above was taken against a
#     warm target tree -- the alias rows against the shared target/, the as-shipped row against
#     its isolated stand-in. Of the labels=['lint'] resources this is the only one whose cargo work
#     lands in the shared target/ -- clippy uses target/clippy, probe-pin-check target/probe-pin,
#     probe-pin-e2e target/probe-pin-e2e, check-frontend runs no cargo at all. BUT THE LABEL IS
#     NOT THE OPERATIVE SET: everything with auto_init = True also runs under `tilt up -- lint`,
#     and 'draft-pools' (auto_init = True) runs `cargo run --bin draft-pool-gen` with NO
#     CARGO_TARGET_DIR, dev profile, on draft-core -- which depends on phase-engine. So the
#     shared tree is PARTIALLY warmed at init: phase-engine's lib and its dependency graph get
#     built there. What is NOT warmed is the part this resource pays for -- the integration test
#     binary, the engine's dev-dependencies, and anything only `build-native` builds
#     (auto_init = 'test' in enabled, so it does not run under a lint-only profile).
#     THE SIZE OF THE RESIDUAL IS UNMEASURED, and no figure in this comment bounds it: a cold
#     `nextest --no-run` over these packages is both a different command and a FULLY cold tree,
#     which is not the state a lint-only `tilt up` leaves. Re-pricing this under a real
#     lint-only `tilt up` settles the number; it is owed, not measured.
#  2. THE 47.2s ASSUMES SEQUENTIAL ORDERING, which nothing now imposes. It was timed as
#     build-native then probe-pin -- the order `resource_deps` would have forced, and there is
#     deliberately no `resource_deps` (below). Both are allow_parallel and both fire on a
#     crates/engine/src/ edit, so the inner `cargo test --no-run` can queue on the SHARED build
#     lock behind build-native's ~82s nextest. The clippy comment above states the same
#     mechanism from the other side: a separate CARGO_TARGET_DIR "gives it its own build lock,
#     so it never queues behind the native test builds". The TOOL build here does have such a dir;
#     the INNER engine resolve does not, by the choice in the next paragraph -- and it is the inner
#     one that queues. 47.2s is therefore a floor, not a ceiling. Under a lint-only
#     profile the contender is not build-native (which never starts) but 'card-data': auto_init
#     = True, deps = ENGINE_SRC, and gen-card-data.sh runs `cargo build --profile tool` into
#     ${CARGO_TARGET_DIR:-target}/tool -- a different profile dir in the SAME target root, and
#     the cargo build lock is per target ROOT (the 'wasm' comment above states this).
#
# The tool is built into target/probe-pin-census (its own dir, see the cmd comment) and then
# invoked as a BINARY. The split is deliberate and it is the SHELL that draws it: a prefix
# assignment binds only the command it prefixes, so CARGO_TARGET_DIR covers `cargo build` and is
# already unset by the time the binary runs. probe-pin's own inner `cargo test --no-run` therefore
# INHERITS an unset CARGO_TARGET_DIR and resolves the integration binary out of the SHARED target/
# -- the same tree every other cargo resource builds into, rather than a second private one.
# Putting CARGO_TARGET_DIR on the `cargo probe-pin` alias instead would push a second, cold engine
# build into target/probe-pin-census. What warms the shared tree is whatever else
# happened to run; this resource orders itself behind nothing, which is hazard 2.
# NO `resource_deps`. MEASURED, not preferred: 'build-native' auto-inits only under the `test`
# profile and this resource only under `lint` (plus the platform gate, which only narrows it
# further), so under `tilt up -- lint` it would wait forever on a resource that never starts --
# merged, green in the file, and enforced NOWHERE, in the only venue this manifest has.
# The PROFILES are named rather than the two `auto_init` expressions quoted: the argument rests
# only on neither condition implying the other, and a quoted expression is a second copy to keep
# in sync -- which is exactly how this sentence came to misquote the resource below.
#
# Every other resource_deps pair in this Tiltfile satisfies "dependent auto-inits =>
# dependency auto-inits" (test-engine/test-ai -> build-native, same group; test-frontend ->
# wasm and coverage -> card-data, dependency always inits; caddy -> frontend violates it only
# under `tilt up -- https tauri`, which nothing rejects, so that pair is already a violation
# and this one would be the second -- the first that fires under a profile the file itself
# documents). Price of not depending on it: build-native saves 1.7s
# of 32.9s on the inner resolve WHEN THE TWO RUN IN SEQUENCE (see the numbers above) -- not worth
# a venue that silently does not run. The parallel case is hazard 2, and it is the price of this
# choice, stated rather than netted out.
#
# PLATFORM: Linux-only, under the file-wide probe-pin policy and for the reason that policy
# states -- this cmd is `probe-pin check`, and that reaches `unshare` unconditionally (see the
# IS_LINUX note at the top of this file for the two coordinates). What the gate BUYS is only
# that a Darwin host does not boot this resource into a red it can never clear; what it does NOT
# buy is any enforcement of the pin off Linux. There is no CI venue to fall back to -- MEASURED:
# `grep -rn probe-pin .github/workflows/` returns nothing, which is the state 'probe-pin-check''s
# note calls a hard stop -- so off Linux this pin's verdict is carried entirely by whatever Linux
# venue last ran it. That is the identical shape 'probe-pin-e2e' states for Tier 2, and it is
# stated here rather than inferred from the flag.
local_resource('probe-pin-census',
    cmd = ['bash', '-c',
           # Starlark has NO implicit adjacent-string-literal concatenation (a Python rule this
           # paste inherited): the `+` is load-bearing, not style. Without it the whole file
           # fails to LOAD -- `tilt alpha tiltfile-result` exits 5 at this line -- which takes
           # every other resource down with it, not just this one.
           # Its OWN CARGO_TARGET_DIR, for the reason 'probe-pin-check' states two resources up:
           # this resource and that one both watch 'crates/probe-pin/', both are allow_parallel,
           # and a shared dir lets one relink the binary the other is mid-execution on.
           'CARGO_TARGET_DIR=target/probe-pin-census cargo build -q -p probe-pin && ' +
           'target/probe-pin-census/debug/probe-pin check probe-pin/engine-census.toml'],
    # deps watches everything that can move THIS RESOURCE'S VERDICT **by changing what the pin
    # measures**: census()'s two walk roots, the tool, the manifest, the pinned test file, the module
    # declaration that puts that test in the binary, and the toolchain the digest covers --
    # deliberately NOT ENGINE_SRC + AI_SRC.
    # ⚠ THE PREDICATE IS STATED BECAUSE THE EARLIER WORD WAS "EVERYTHING", AND THAT WAS FALSE.
    # Disclosed residual, not a gap that was missed: the target is resolved by building
    # `--test integration`, so ANY sibling file under crates/engine/tests/integration/ can move the
    # verdict -- by breaking that build, or by adding a test that FAILS under the manifest's
    # control. Those are NOT watched, on purpose: watching them would re-trigger this pin on every
    # unrelated integration-test edit, and there are over a thousand of them. Both failure classes
    # surface as exit 2 with the cause named in the output. THE DEV-DEP AND LOCKFILE HALF OF THIS
    # RESIDUAL IS NO LONGER RESIDUAL -- it was listed here alongside the thousand siblings and
    # inherited their exemption, which was wrong by this file's own admission rule: a lockfile-only
    # edit leaves a stale GREEN, and Cargo.lock is one file with a tiny edit surface. It, the two
    # manifests that reach this target, and .cargo/config.toml are watched below.
    # The pin is over exactly what census() reads; ENGINE_SRC is a superset BY DESIGN (its own
    # comment requires it to stay a superset of the engine cache key: src + data + build.rs +
    # Cargo.toml), and census() reads none of the extras. The live cost of the wider set is not
    # hypothetical: gen-card-data.sh PROMOTES tracked files under crates/engine/data/, and
    # 'card-data' (auto_init = True, so under EVERY profile) documents that watching its own
    # outputs "makes every promote re-trigger card-data -> an infinite regen loop" and ignores
    # them for that reason. This resource's ignore list does not cover them, so with ENGINE_SRC
    # every promote would re-run the whole probe set at full price for a pin that cannot move.
    # AI_SRC happens to EQUAL its walk root today; not depending on it is the same point --
    # "what makes the engine rebuild" and "what census() reads" are different concepts that
    # currently share a value, and a later path added to either symbol re-opens this silently.
    deps = [
        'crates/engine/src/',
        'crates/phase-ai/src/',
        # THE BUILD INPUTS OF THE TARGET THIS PIN RESOLVES BY BUILDING (`--test integration`).
        # A lockfile- or manifest-only edit changes what that target compiles while every watched
        # source file stays byte-identical, so without these the resource keeps its PRIOR GREEN
        # even when the rebuilt target would fail or move a number. Each is admitted by the rule
        # 'main.rs' below is admitted by -- ONE file with a tiny, stable edit surface -- not by the
        # "everything that can break the build" reading the comment above already rejects for the
        # thousand-odd sibling test files.
        # WHICH manifests, MEASURED not shotgunned (`cargo tree -p phase-engine -e normal,dev`
        # intersected with the workspace members): phase-engine is the ONLY workspace member in
        # this target's graph, so its manifest plus the workspace root's -- [profile.test] and
        # [workspace.dependencies], which this target resolves through -- are the whole set.
        # Sibling crate manifests are deliberately absent: they cannot reach this build.
        # probe-pin's own manifest already rides along inside the 'crates/probe-pin/' dep below.
        # Cargo may itself rewrite Cargo.lock when it is stale, which costs at most ONE extra
        # settling run, not a loop: the rewrite is idempotent.
        'Cargo.lock',
        'Cargo.toml',
        'crates/engine/Cargo.toml',
        # NOT a manifest, watched on the same one-file rule: [env] here sets
        # RUST_MIN_STACK = 16777216, which that file records as load-bearing for test threads
        # (Debug-formatting the Effect <-> AbilityDefinition recursion overflows a default
        # stack). Lowering it turns these tests red with no source edit at all.
        '.cargo/config.toml',
        # The TOOL that renders the block, watched for the same reason 'probe-pin-check' watches
        # it: a change to block::render or the digest moves the rendered block, and without this
        # nothing re-triggers the check until an unrelated engine edit happens to fire it.
        'crates/probe-pin/',
        'probe-pin/engine-census.toml',
        'crates/engine/tests/integration/loop_shortcut_offer_writer_census.rs',
        # The MODULE DECLARATION. Deleting `mod loop_shortcut_offer_writer_census;` from main.rs
        # takes the pinned test out of the binary. MEASURED, not reasoned: three siblings already
        # import from `super::loop_shortcut_offer_writer_census` (two take
        # `{cfg_test_scoped_lines, rs_files}`, one takes `rs_files`), so that deletion is an
        # E0432 BUILD failure, not the zero-selection execution floor -- `check`
        # aborts either way and every Abort maps to exit 2. It is watched not because its failure is
        # quieter than a sibling's (it is the same class) but because it is ONE file with a tiny,
        # stable edit surface: the cost of watching it rounds to zero, which is exactly what is not
        # true of the thousand-odd siblings above.
        'crates/engine/tests/integration/main.rs',
        # The TOOLCHAIN, for the same reason and by the same rule. `used_instruments()` returns
        # [Instrument::Toolchain] UNCONDITIONALLY and the digest covers that list, so a rustc move
        # with NO measured number changing is still `check` exit 1 (reported as an instrument
        # change, not code drift). `channel` here is date-pinned, so in this repo a rustc move IS
        # an edit to this file: watching it puts the red on the commit that caused it instead of
        # on the next unrelated engine edit, where it would read as census drift. It does not
        # cover a rustc moved WITHOUT this file (`rustup override`, RUSTUP_TOOLCHAIN).
        'rust-toolchain.toml',
    ],
    # '**/tmp/**' rides along with the 'crates/probe-pin/' dep, NOT as boilerplate: TMP_IGNORE is
    # a FILENAME glob ('**/*.tmp.*') that does not match a tmp/ DIRECTORY, and probe-pin's Tier-2
    # tests scratch-write under crates/probe-pin/tests/fixtures/tmp/. 'probe-pin-check' carries
    # this exact pairing and states the reason; watching the crate without it re-imports the
    # retrigger loop that comment exists to document.
    ignore = TMP_IGNORE + ['**/tmp/**'],
    auto_init = 'lint' in enabled and IS_LINUX,
    trigger_mode = PROBE_PIN_TRIGGER,
    allow_parallel = True,
    labels = ['lint'],
)
