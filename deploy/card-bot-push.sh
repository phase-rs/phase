#!/usr/bin/env bash
set -euo pipefail

# Build the card-bot image locally, ship it to the VPS over SSH, and (re)start
# the container. Mirrors deploy/push.sh (phase-server), but auto-detects how the
# SSH user reaches docker: directly if it can, else via `sudo docker`. (A fresh
# setup-vps.sh box grants passwordless `sudo docker`; this box puts the deploy
# user in the docker group instead.)
#
# The long-lived container needs NO secrets: the public key is baked into
# config.ts and signature follow-ups use the per-interaction webhook token. The
# bot token is only needed to (re)register the slash command — a one-off
# `docker run` reading an env file on the host (app id / guild id / public key
# are all baked-in defaults, so the token is all that's required):
#   /etc/phase-card-bot.env  →  CARD_BOT_TOKEN   (secret, from the Discord portal)
#
# Usage: ./deploy/card-bot-push.sh        (HOST defaults to the phase-vps ssh alias)

HOST="${CARD_BOT_HOST:-phase-vps}"
IMAGE="phase-card-bot:local"
ENV_FILE="/etc/phase-card-bot.env"

# Remote prelude: choose `docker` vs `sudo docker` for this SSH user.
detect='D=docker; docker info >/dev/null 2>&1 || D="sudo docker";'

wait_for_health_remote='for _ in $(seq 1 30); do
  if curl -fsS http://127.0.0.1:9375/health >/dev/null; then healthy=1; break; fi
  sleep 1
done
if [ "${healthy:-0}" != "1" ]; then
  $D logs --tail 50 phase-card-bot || true
  exit 1
fi'

echo "Building ${IMAGE}..."
# --platform linux/amd64: the VPS is x86_64 even when building from Apple Silicon.
# --provenance=false keeps the image in the classic format the host's older
# Docker (20.10.x) can `docker load`.
docker buildx build --platform linux/amd64 --provenance=false --load \
  -f deploy/card-bot.Dockerfile -t "$IMAGE" scripts/card-bot

echo "Uploading image to ${HOST}..."
docker save "$IMAGE" | ssh "${HOST}" "${detect} \$D load"

echo "Deploying..."
ssh "${HOST}" "${detect} \
  (\$D stop phase-card-bot || true) \
  && (\$D rm phase-card-bot || true) \
  && \$D run -d \
    --name phase-card-bot \
    --restart unless-stopped \
    -p 127.0.0.1:9375:9375 \
    ${IMAGE} \
  && echo 'Waiting for health...' \
  && ${wait_for_health_remote} \
  && \$D ps --filter name=phase-card-bot --filter status=running"

echo "Done — phase-card-bot deployed to ${HOST}"
echo "If the /card command shape changed, register it once with:"
echo "  ssh ${HOST} \"${detect} \\\$D run --rm --env-file ${ENV_FILE} ${IMAGE} bun run card-bot/register.ts\""
