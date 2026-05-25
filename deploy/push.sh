#!/usr/bin/env bash
set -euo pipefail

# Quick-deploy phase-server container image to VPS for testing.
# Builds the Docker image locally, streams it to the host, and restarts the
# phase-server container.

HOST="phase-vps"
IMAGE="phase-server:local"

wait_for_health_remote='for attempt in $(seq 1 30); do
  if curl -fsS http://127.0.0.1:9374/health; then
    healthy=1
    break
  fi
  sleep 1
done
if [ "${healthy:-0}" != "1" ]; then
  sudo docker logs --tail 50 phase-server || true
  exit 1
fi'

echo "Building ${IMAGE}..."
docker buildx build --load -t "$IMAGE" .

echo "Uploading image to ${HOST}..."
docker save "$IMAGE" | ssh "${HOST}" "sudo docker load"

echo "Deploying..."
ssh "${HOST}" "\
  (sudo systemctl stop phase-server || true) \
  && (sudo systemctl disable phase-server || true) \
  && (sudo systemctl mask phase-server || true) \
  && (sudo docker stop --time 30 phase-server || true) \
  && (sudo docker rm phase-server || true) \
  && sudo docker volume create phase-server-data >/dev/null \
  && sudo docker run -d \
    --name phase-server \
    --restart unless-stopped \
    -p 127.0.0.1:9374:9374 \
    -v phase-server-data:/var/lib/phase-server \
    -e PHASE_LOBBY_ONLY=true \
    -e PHASE_CORS_ORIGIN='*' \
    -e RUST_LOG=info \
    ${IMAGE} \
  && echo 'Service status:' \
  && ${wait_for_health_remote} \
  && sudo docker ps --filter name=phase-server --filter status=running"

echo "Done — phase-server deployed to ${HOST}"
