#!/usr/bin/env bash
set -euo pipefail

# Deploy phase-server from GHCR release image
# Usage: ./deploy.sh [version]
#   version: tag like "v0.1.0" (default: latest release)

REPO="phase-rs/phase"
IMAGE_REPO="ghcr.io/phase-rs/phase-server"

wait_for_health() {
    for _ in $(seq 1 30); do
        if curl -fsS http://127.0.0.1:9374/health; then
            echo ""
            return 0
        fi
        sleep 1
    done

    sudo docker logs --tail 50 phase-server || true
    return 1
}

VERSION="${1:-latest}"

if [ "$VERSION" = "latest" ]; then
    echo "Fetching latest release..."
    VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | jq -r .tag_name)
fi

echo "Deploying phase-server ${VERSION}..."

IMAGE="${IMAGE_REPO}:${VERSION}"

if [ -n "${GHCR_TOKEN:-}" ]; then
    : "${GHCR_USER:?Set GHCR_USER when GHCR_TOKEN is set}"
    echo "Logging in to GHCR as ${GHCR_USER}..."
    echo "$GHCR_TOKEN" | sudo docker login ghcr.io -u "$GHCR_USER" --password-stdin
fi

echo "Pulling ${IMAGE}..."
sudo docker pull "$IMAGE"

echo "Stopping legacy systemd service if present..."
sudo systemctl stop phase-server 2>/dev/null || true
sudo systemctl disable phase-server 2>/dev/null || true
sudo systemctl mask phase-server 2>/dev/null || true

echo "Starting container..."
sudo docker stop --time 30 phase-server 2>/dev/null || true
sudo docker rm phase-server 2>/dev/null || true
sudo docker volume create phase-server-data >/dev/null
sudo docker run -d \
    --name phase-server \
    --restart unless-stopped \
    -p 127.0.0.1:9374:9374 \
    -v phase-server-data:/var/lib/phase-server \
    -e PHASE_LOBBY_ONLY=true \
    -e PHASE_CORS_ORIGIN='*' \
    -e RUST_LOG=info \
    "$IMAGE"

echo "Checking health..."
wait_for_health
sudo docker ps --filter name=phase-server --filter status=running

echo "Deploy complete: ${IMAGE}"
