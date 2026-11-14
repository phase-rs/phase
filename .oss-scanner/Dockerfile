# Scanner image for https://github.com/phase-rs/phase.
# Enrolment: anthropics/oss-scanner `projects/phase/project.yaml` points
# here (`dockerfile: .oss-scanner/Dockerfile`) once that PR points at
# this in-repo copy; until then the scanner uses the copy beside
# project.yaml. Keep the two Dockerfiles and threat models in sync, or
# delete the scanner-repo copies after the pointer switch.
# Dockerfile.dockerignore, next to this file, keeps data/ (including the
# test fixture the engine tests embed) visible to this build; the root
# .dockerignore is written for phase's production images and hides it.
#
# The audit runs offline, so this build installs the pinned toolchain,
# fetches every crate the workspace resolves, and compiles the native
# (non-WASM) server stack and its test binaries. threat_model.md, next
# to this file, says how to exercise each piece.
#
# Scope: the Rust workspace. The React/TypeScript client under client/
# is not built here (its production build also compiles the engine to
# WASM); it is audited as source. The network-facing server, the rules
# engine and every parser of untrusted input are native Rust and are
# built below.

FROM rust:1-bookworm

ENV DEBIAN_FRONTEND=noninteractive

# build-essential + pkg-config for the bundled SQLite in rusqlite and
# other -sys crates; git for crates/server-core/build.rs (it embeds the
# short commit and falls back to "dev" without a checkout).
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential \
      ca-certificates \
      git \
      libssl-dev \
      pkg-config \
      python3 \
    && rm -rf /var/lib/apt/lists/*

# scanner contract: the checkout lives inside the image, at /src
COPY . /src
WORKDIR /src

# The workspace pins its toolchain in rust-toolchain.toml
# (nightly-2026-04-19, with the cranelift backend and the
# wasm32-unknown-unknown target). Install it explicitly so a bad pin
# fails here, while the network is up, not mid-build.
RUN rustup toolchain install \
    && rustup target list --installed \
    && cargo --version

# Bound the build's memory on smaller machines; the scanner's build
# host is larger, but the enrolment CI runner is not.
ARG CARGO_BUILD_JOBS=4

# Every crate the workspace depends on, fetched while the network is
# still available.
RUN cargo fetch

# Build the server stack as its maintainers build it: the WebSocket
# server binary (phase-server), the wire-protocol guards and session
# logic (server-core, lobby-broker, seat-reducer), the draft logic
# (draft-core), the AI seats (phase-ai) and the rules engine
# (phase-engine; its library target is named `engine`).
# Debug info and frame pointers keep audit stack traces readable.
ENV RUSTFLAGS="-C force-frame-pointers=yes"
RUN cargo build -p phase-server -p server-core -p lobby-broker \
    && cargo test -p phase-engine -p server-core -p lobby-broker \
         -p draft-core -p seat-reducer --no-run

# Run the fast, security-adjacent suites (the wire guards, draft and
# lobby validation) so the log shows the image works. A failure warns
# but does not abort the build; the full engine suite is large and is
# run selectively by the auditor, per threat_model.md.
RUN (timeout 1200 cargo test -p server-core -p lobby-broker \
       -p draft-core -p seat-reducer \
     || echo "WARNING: cargo test reported failures")

# Verify the documented fixture-backed server startup without network or a
# generated production card-data.json. PHASE_DEV_FIXTURE=1 loads the
# committed data/mtgjson/test_fixture.json and disables the download
# bootstrap; /health must answer on the smoke-test port.
RUN set -eu; \
    PHASE_DEV_FIXTURE=1 ./target/debug/phase-server \
      --bind 127.0.0.1 --port 19374 --games-db /tmp/phase-server-smoke.db \
      >/tmp/phase-server-smoke.log 2>&1 & \
    pid=$!; \
    ok=0; \
    for _ in $(seq 1 30); do \
      if python3 -c "import urllib.request; raise SystemExit(0 if urllib.request.urlopen('http://127.0.0.1:19374/health', timeout=1).read() == b'ok' else 1)" 2>/dev/null; then \
        ok=1; break; \
      fi; \
      sleep 1; \
    done; \
    kill "$pid" 2>/dev/null || true; \
    wait "$pid" 2>/dev/null || true; \
    if [ "$ok" != 1 ]; then cat /tmp/phase-server-smoke.log; exit 1; fi

# Everything is fetched and built. From here on cargo must not wait
# for a registry it cannot reach.
ENV CARGO_NET_OFFLINE=true
