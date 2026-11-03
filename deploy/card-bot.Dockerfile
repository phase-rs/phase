# syntax=docker/dockerfile:1
#
# Discord /card parse-breakdown and /lfg matchmaking bot. Dependency-free Bun
# service — it fetches coverage-data.json from R2 and card images from Scryfall at
# runtime, so no data or npm packages are baked in. Build context is
# the repository root:
#   docker build -f deploy/card-bot.Dockerfile -t phase-card-bot .
#
# /lfg state (open posts, seats, room codes) lives in SQLite under /data — mount a
# named volume there (card-bot-push.sh does) so it survives redeploys. A fresh
# named volume is initialized from the image's /data, so it starts owned by bun.

FROM oven/bun:1.3-alpine AS protocols
WORKDIR /source
COPY protocol/ ./protocol/
COPY scripts/protocol-versions.mjs ./scripts/protocol-versions.mjs
RUN bun scripts/protocol-versions.mjs generate

FROM oven/bun:1.3-alpine AS runtime

WORKDIR /app/scripts
COPY --chown=bun:bun scripts/card-bot/ ./card-bot/
COPY --from=protocols --chown=bun:bun /source/client/src/generated/protocol-versions.ts /app/client/src/generated/protocol-versions.ts

ENV CARD_BOT_PORT=9375
EXPOSE 9375

RUN mkdir -p /data && chown bun:bun /data
ENV CARD_BOT_DB_PATH=/data/lfg.sqlite
VOLUME /data

USER bun

# Generous start period: the first request warms the ~52MB coverage export.
HEALTHCHECK --interval=30s --timeout=5s --start-period=40s --retries=3 \
    CMD wget -qO- "http://127.0.0.1:${CARD_BOT_PORT}/health" >/dev/null 2>&1 || exit 1

CMD ["bun", "run", "card-bot/index.ts"]
