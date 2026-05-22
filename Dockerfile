# syntax=docker/dockerfile:1

FROM rust:slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    libssl-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY . .

RUN cargo build --profile server-release --bin phase-server

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    gosu \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system phase \
    && useradd --system --gid phase --home-dir /var/lib/phase-server --shell /usr/sbin/nologin phase

COPY --from=builder /app/target/server-release/phase-server /usr/local/bin/phase-server
COPY docker/phase-server-entrypoint.sh /usr/local/bin/phase-server-entrypoint

RUN --mount=type=bind,source=data,target=/context-data,readonly \
    mkdir -p /usr/share/phase-server \
    && cp /context-data/card-data.json /usr/share/phase-server/card-data.json \
    && if [ -f /context-data/draft-pools.json ]; then \
        cp /context-data/draft-pools.json /usr/share/phase-server/draft-pools.json; \
    else \
        printf '{}\n' > /usr/share/phase-server/draft-pools.json; \
    fi

RUN mkdir -p /var/lib/phase-server \
    && chown -R phase:phase /var/lib/phase-server \
    && chmod +x /usr/local/bin/phase-server-entrypoint

ENV PORT=9374
ENV PHASE_DATA_DIR=/var/lib/phase-server
ENV RUST_LOG=info

EXPOSE 9374

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD sh -c 'curl -fsS "http://127.0.0.1:${PORT}/health" >/dev/null'

ENTRYPOINT ["phase-server-entrypoint"]
CMD ["phase-server"]
