# syntax=docker/dockerfile:1

# BINARY_STAGE selects how the phase-server binary arrives in the runtime image:
#   - "compile"  (default): build it from source inside Docker. Used by
#     release.yml's build-server-image and any plain `docker build`.
#   - "prebuilt": copy a binary already compiled on the host. CI builds a static
#     musl binary natively with a warm cargo cache (far faster than a cold
#     in-container release build), then passes --build-arg BINARY_STAGE=prebuilt.
#     The static musl binary has no glibc dependency, so it runs on the slim
#     debian runtime regardless of the host's glibc version. See deploy.yml.
ARG BINARY_STAGE=compile

FROM rust:slim-bookworm AS compile

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    libssl-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY . .

RUN cargo build --profile server-release --bin phase-server \
    && cp target/server-release/phase-server /phase-server

# Prebuilt path: expects ./phase-server (a static musl binary) in the build
# context. Only built when BINARY_STAGE=prebuilt; otherwise BuildKit skips it.
FROM debian:bookworm-slim AS prebuilt
COPY phase-server /phase-server

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
