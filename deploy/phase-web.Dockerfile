# syntax=docker/dockerfile:1
#
# The phase web client, for the phase-server chart's `web.enabled` sidecar.
# Build the payload first with scripts/build-selfhost-web.sh, which leaves a
# ready-to-copy tree in client/dist:
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
# nginx.conf is deliberately NOT baked in: the chart mounts its own from a
# ConfigMap, so one image works for every deployment. Same for /config.js — the
# copy here is the empty placeholder from client/public, and the chart serves its
# own over it.
# Pinned to the multi-arch index digest, not just the tag, so a registry-side
# retag cannot change the nginx shipped to every deployment built from this
# file. Same image and same digest as the chart's logging sidecar
# (`logging.server.image` in values.yaml) — keep the two in step.
# Re-resolve with: docker buildx imagetools inspect nginxinc/nginx-unprivileged:<tag>
FROM nginxinc/nginx-unprivileged:1.27-alpine@sha256:65e3e85dbaed8ba248841d9d58a899b6197106c23cb0ff1a132b7bfe0547e4c0

COPY dist/ /usr/share/nginx/html/
