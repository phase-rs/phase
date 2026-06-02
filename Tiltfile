# phase.rs — local development orchestration
#
# Usage:
#   tilt up                              core dev loop (wasm + frontend)
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

ENGINE_SRC = ['crates/engine/src/']
AI_SRC = ['crates/phase-ai/src/']
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
    cmd = 'CARGO_TARGET_DIR=target/wasm ./scripts/build-wasm.sh',
    deps = ENGINE_SRC + AI_SRC + WASM_SRC + DRAFT_CORE_SRC + DRAFT_WASM_SRC,
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

TAURI_SRC = ['client/src-tauri/src/']
SIDECAR_DEST = 'client/src-tauri/binaries/phase-server-' + str(local('rustc -vV | sed -n "s/host: //p" | tr -d "\\n"', quiet = True))

local_resource('tauri',
    cmd = 'cargo build -p phase-server && mkdir -p client/src-tauri/binaries && cp target/debug/phase-server ' + SIDECAR_DEST,
    serve_cmd = 'pnpm tauri:dev',
    serve_dir = 'client',
    deps = ENGINE_SRC + AI_SRC + WASM_SRC + TAURI_SRC + ['crates/server-core/src/', 'crates/phase-server/src/'],
    auto_init = 'tauri' in enabled,
    labels = ['serve'],
)

SERVER_SRC = ENGINE_SRC + AI_SRC + [
    'crates/server-core/src/',
    'crates/phase-server/src/',
]

local_resource('server',
    cmd = 'cargo build --bin phase-server',
    serve_cmd = './target/debug/phase-server',
    serve_env = {'PHASE_DATA_DIR': 'data'},
    deps = SERVER_SRC,
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
# binaries without executing them; the downstream `cargo test -p ...` then finds
# everything fingerprint-fresh and just runs (no recompile). Default features —
# matching the test resources, which (unlike `cargo test-all`) do not enable
# engine/proptest; a feature mismatch here would force a rebuild.
local_resource('build-native',
    cmd = 'cargo test -p engine -p phase-ai --no-run',
    deps = ENGINE_SRC + AI_SRC,
    allow_parallel = True,
    auto_init = 'test' in enabled,
    labels = ['test'],
)

local_resource('test-engine',
    cmd = 'cargo test -p engine',
    deps = ENGINE_SRC,
    resource_deps = ['build-native'],
    allow_parallel = True,
    auto_init = 'test' in enabled,
    labels = ['test'],
)

local_resource('test-ai',
    cmd = 'cargo test -p phase-ai',
    deps = ENGINE_SRC + AI_SRC,
    resource_deps = ['build-native'],
    allow_parallel = True,
    auto_init = 'test' in enabled,
    labels = ['test'],
)

local_resource('test-frontend',
    cmd = 'pnpm test -- --run',
    dir = 'client',
    deps = ['client/src/'],
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
    cmd = 'CARGO_TARGET_DIR=target/clippy cargo clippy --all-targets -- -D warnings',
    deps = ['crates/'],
    auto_init = 'lint' in enabled,
    allow_parallel = True,
    labels = ['lint'],
)

local_resource('check-frontend',
    cmd = 'pnpm run type-check && pnpm lint',
    dir = 'client',
    deps = ['client/src/'],
    allow_parallel = True,
    auto_init = 'lint' in enabled,
    labels = ['lint'],
)

# ---------------------------------------------------------------------------
# Data (manual trigger — click in UI to run)
# ---------------------------------------------------------------------------

local_resource('card-data',
    cmd = './scripts/gen-card-data.sh',
    deps = ENGINE_SRC,
    auto_init = True,
    labels = ['data'],
)

local_resource('draft-pools',
    cmd = 'cargo run --bin draft-pool-gen',
    deps = DRAFT_CORE_SRC + ['data/mtgjson/sets/'],
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
