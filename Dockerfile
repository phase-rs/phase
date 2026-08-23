# syntax=docker/dockerfile:1

# BINARY_STAGE selects how the phase-server binary arrives in the runtime image:
#   - "compile"  (default): cross-compile from source on the BUILD host for each
#     requested platform, so one plain
#       docker buildx build --platform linux/amd64,linux/arm64 .
#     yields a multi-arch image with no emulated cargo run.
#   - "prebuilt": copy ./phase-server-<amd64|arm64> (static musl binaries built
#     outside Docker with a warm cargo cache) from the build context; the image
#     build is then just apt + COPY. Used by release.yml and deploy.yml.
#
# PHASE_CHANNEL (compile stage only; e.g. "release", as release.yml sets for
# its native builds) bakes the data-manifest identity into the binary so an
# empty PHASE_DATA_DIR self-bootstraps card data on first boot. Unset = no
# identity: such an image needs PHASE_DATA_MANIFEST_URL or pre-provisioned data.
ARG BINARY_STAGE=compile

FROM --platform=$BUILDPLATFORM rust:slim-bookworm AS compile
ARG TARGETARCH
ARG PHASE_CHANNEL

# zig is the C cross-compiler/linker for the musl targets (ring, bundled
# SQLite, mimalloc). It runs on any build host, unlike per-target gcc tarballs.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    python3-pip \
    && rm -rf /var/lib/apt/lists/* \
    && pip install --break-system-packages --no-cache-dir cargo-zigbuild==0.23.0 ziglang==0.16.0

WORKDIR /app

COPY . .

# -p scopes feature unification to phase-server's own graph: unscoped, the
# workspace unifies feed-scraper's native-tls reqwest features in, dynamically
# linking OpenSSL — which the musl target and the slim runtime image lack.
# Every cache is keyed per arch: a multi-platform build runs both legs
# concurrently, and cargo's package-cache lock lives in $CARGO_HOME itself
# (outside these mounts), so two legs sharing one registry dir race on
# unpacking crates (measured: "failed to unpack package … File exists").
# BuildKit's default 1024-fd soft limit is too low for zig/lld to link the
# final binary (ProcessFdQuotaExceeded); the hard limit is far higher.
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=phase-server-cargo-registry-$TARGETARCH \
    --mount=type=cache,target=/usr/local/cargo/git,id=phase-server-cargo-git-$TARGETARCH \
    --mount=type=cache,target=/app/target,id=phase-server-target-$TARGETARCH \
    ulimit -n 65536 \
    && case "$TARGETARCH" in \
      amd64) TARGET=x86_64-unknown-linux-musl ;; \
      arm64) TARGET=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
    esac \
    && rustup target add "$TARGET" \
    && ${PHASE_CHANNEL:+env PHASE_CHANNEL="$PHASE_CHANNEL"} \
       cargo zigbuild -p phase-server --profile server-release --bin phase-server --target "$TARGET" \
    && cp "target/$TARGET/server-release/phase-server" /phase-server

# Prebuilt path: expects ./phase-server-<arch> (static musl) in the build
# context for every platform being built. Only evaluated when
# BINARY_STAGE=prebuilt; otherwise BuildKit skips it.
FROM scratch AS prebuilt
ARG TARGETARCH
COPY phase-server-${TARGETARCH} /phase-server

# Resolve the selected source to a single stage name the runtime copies from.
FROM ${BINARY_STAGE} AS binary

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    gosu \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system phase \
    && useradd --system --gid phase --home-dir /var/lib/phase-server --shell /usr/sbin/nologin phase

COPY --from=binary /phase-server /usr/local/bin/phase-server
COPY docker/phase-server-entrypoint.sh /usr/local/bin/phase-server-entrypoint

RUN mkdir -p /var/lib/phase-server \
    && chown -R phase:phase /var/lib/phase-server \
    && chmod +x /usr/local/bin/phase-server /usr/local/bin/phase-server-entrypoint

ENV PORT=9374
ENV PHASE_DATA_DIR=/var/lib/phase-server
ENV RUST_LOG=info

EXPOSE 9374

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD sh -c 'curl -fsS "http://127.0.0.1:${PORT}/health" >/dev/null'

ENTRYPOINT ["phase-server-entrypoint"]
CMD ["phase-server"]
