#!/bin/bash
# Launch wrapper for Tilt's `caddy` resource. The CADDY_BIND_ADDRESS env
# var is the entire reason :443 works without sudo on macOS: macOS allows
# non-root binding to privileged ports only when the listen address is
# 0.0.0.0 (all interfaces), not 127.0.0.1. On Linux we use loopback addrs
# so docker bridge networks can talk back to the host.
#
# Pattern lifted from torch/archipelago's tilt-scripts/run-caddy.sh.

set -e

CADDY_BIND_ADDRESS=0.0.0.0

if [[ "$OSTYPE" == "linux-gnu"* ]]; then
  CADDY_BIND_ADDRESS="127.0.0.1 [::1]"
fi

exec env CADDY_BIND_ADDRESS="$CADDY_BIND_ADDRESS" caddy run --config Caddyfile --adapter caddyfile
