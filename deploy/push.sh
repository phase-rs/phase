#!/usr/bin/env bash
set -euo pipefail

# Quick-deploy phase-server to VPS for testing.
# Cross-compiles via cargo-zigbuild, SCPs to VPS, restarts service.

HOST="phase-vps"

echo "Building phase-server for linux x86_64..."
cargo zigbuild --release --bin phase-server --target x86_64-unknown-linux-gnu

echo "Uploading binary, card data, and service file..."
scp target/x86_64-unknown-linux-gnu/release/phase-server "${HOST}:/tmp/phase-server"
scp client/public/card-data.json "${HOST}:/tmp/card-data.json"
scp deploy/phase-server.service "${HOST}:/tmp/phase-server.service"

echo "Deploying..."
ssh "${HOST}" "\
  sudo systemctl stop phase-server || true \
  && sudo cp /tmp/phase-server /opt/phase-server/phase-server \
  && sudo chmod +x /opt/phase-server/phase-server \
  && sudo cp /tmp/card-data.json /opt/phase-server/data/card-data.json \
  && sudo cp /tmp/phase-server.service /etc/systemd/system/phase-server.service \
  && sudo systemctl daemon-reload \
  && sudo chown -R phase:phase /opt/phase-server \
  && sudo systemctl start phase-server \
  && rm -f /tmp/phase-server /tmp/card-data.json /tmp/phase-server.service \
  && echo 'Service status:' \
  && sudo systemctl is-active phase-server"

echo "Done — phase-server deployed to ${HOST}"
