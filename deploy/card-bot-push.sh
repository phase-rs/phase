#!/usr/bin/env bash
set -euo pipefail

# Build the card-bot image locally, ship it to the VPS over SSH, and (re)start
# the container. Mirrors deploy/push.sh (phase-server), but auto-detects how the
# SSH user reaches docker: directly if it can, else via `sudo docker`. (A fresh
# setup-vps.sh box grants passwordless `sudo docker`; this box puts the deploy
# user in the docker group instead.)
#
# Both the long-lived container and the one-off command registration read the
# host's env file (app id / guild id / public key are baked-in defaults, so the
# token is all it needs):
#   /etc/phase-card-bot.env  →  CARD_BOT_TOKEN   (secret, from the Discord portal)
# The deploy refuses to replace the running container without a readable env
# file. The server uses the token only for /lfg game threads (a
# file without CARD_BOT_TOKEN runs the bot with threads off, pinging players
# under the post instead). Threads need the bot's role to have Create Private
# Threads, Send Messages in Threads and Manage Threads in the /lfg channel.
#
# /lfg state persists across redeploys in the named volume phase-card-bot-data,
# mounted at /data (the image's CARD_BOT_DB_PATH is /data/lfg.sqlite).
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

cd "$(dirname "$0")/.."

# The seam pins (formats, endpoints, server directory) guard what the image
# ships; a failure aborts before anything is built (set -e).
echo "Testing card-bot..."
bun test scripts/card-bot

echo "Building ${IMAGE}..."
# --platform linux/amd64: the VPS is x86_64 even when building from Apple Silicon.
# --provenance=false keeps the image in the classic format the host's older
# Docker (20.10.x) can `docker load`.
docker buildx build --platform linux/amd64 --provenance=false --load \
  -f deploy/card-bot.Dockerfile -t "$IMAGE" scripts/card-bot

echo "Uploading image to ${HOST}..."
docker save "$IMAGE" | ssh "${HOST}" "${detect} \$D load"

echo "Deploying..."
# The first `run` is a dry run of the env file: the docker CLI reads --env-file
# itself (as the SSH user, or root under sudo docker), so this fails exactly
# when the real run would, but before the old container is stopped.
ssh "${HOST}" "${detect} \
  (\$D run --rm --env-file ${ENV_FILE} --entrypoint true ${IMAGE} \
    || { echo 'error: ${ENV_FILE} is missing or unreadable; the running bot is untouched' >&2; exit 1; }) \
  && (\$D stop phase-card-bot || true) \
  && (\$D rm phase-card-bot || true) \
  && \$D run -d \
    --name phase-card-bot \
    --restart unless-stopped \
    --env-file ${ENV_FILE} \
    -p 127.0.0.1:9375:9375 \
    -v phase-card-bot-data:/data \
    ${IMAGE} \
  && echo 'Waiting for health...' \
  && ${wait_for_health_remote} \
  && \$D ps --filter name=phase-card-bot --filter status=running"

echo "Done — phase-card-bot deployed to ${HOST}"
echo "If a command shape changed (/card or /lfg), register once with:"
echo "  ssh ${HOST} \"${detect} \\\$D run --rm --env-file ${ENV_FILE} ${IMAGE} bun run card-bot/register.ts\""
