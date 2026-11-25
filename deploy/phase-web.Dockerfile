# syntax=docker/dockerfile:1
#
# The phase web client: the phase-server chart's `web.enabled` sidecar, or a
# standalone container. Build the payload first with
# scripts/build-selfhost-web.sh, which leaves a ready-to-copy tree in
# client/dist:
#
#   ./scripts/build-selfhost-web.sh
#   docker buildx build -f deploy/phase-web.Dockerfile \
#     --platform linux/amd64,linux/arm64 -t <repo>/phase-web:<tag> --push client
#
# KEEP THIS RUN-FREE. With only FROM and COPY there is nothing to execute in the
# target rootfs, so buildx assembles both architectures on either host with no
# qemu emulation — the SPA payload is arch-independent bytes. Adding a single RUN
# step silently reintroduces a binfmt/qemu dependency, which breaks arm64 builds
# on an amd64 host (and needs setup-qemu-action in CI).
#
# One image works for every deployment. Standalone, it is configured through the
# environment and needs no mounted files:
#
#   docker run -p 8080:8080 \
#     -e PHASE_MULTIPLAYER_SERVER_URL=wss://phase.example.com/ws <repo>/phase-web:<tag>
#
# client/deploy/phase-web/nginx.conf is the default config, and
# 40-phase-web-config.sh renders /config.js from PHASE_* variables at start (see
# that script for the list). The chart mounts its own nginx.conf and config.js
# from a ConfigMap over these, so it behaves as before. The bundle's
# dist/config.js is the empty placeholder from client/public and is not served.
#
# Pinned to the multi-arch index digest, not just the tag, so a registry-side
# retag cannot change the nginx shipped to every deployment built from this
# file. Same image and same digest as the chart's logging sidecar
# (`logging.server.image` in values.yaml) — keep the two in step.
# Re-resolve with: docker buildx imagetools inspect nginxinc/nginx-unprivileged:<tag>
FROM nginxinc/nginx-unprivileged:1.27-alpine@sha256:65e3e85dbaed8ba248841d9d58a899b6197106c23cb0ff1a132b7bfe0547e4c0

# --chmod rather than relying on the checkout's file modes, which a Windows clone
# does not keep: /docker-entrypoint.sh skips a script without its exec bit.
COPY --chmod=0644 deploy/phase-web/nginx.conf /etc/nginx/nginx.conf
COPY --chmod=0755 deploy/phase-web/40-phase-web-config.sh /docker-entrypoint.d/

COPY dist/ /usr/share/nginx/html/
