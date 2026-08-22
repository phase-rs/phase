# phase-server Helm chart

Runs one `phase-server` pod behind a Traefik Ingress with a cert-manager TLS
certificate. Mirrors `deploy/deploy.sh` (data volume at `/var/lib/phase-server`,
`/health` probes) but with the hardening Kubernetes makes cheap.

```bash
helm install phase-server deploy/helm/phase-server -n phase --create-namespace \
  --set ingress.host=phase.example.com \
  --set ingress.tls.clusterIssuer=letsencrypt \
  --set networkPolicy.ingressNamespaceLabels."kubernetes\.io/metadata\.name"=kube-system  # your Traefik's namespace
```

Players enter `wss://phase.example.com/ws` in the client's Server picker.

## What the chart assumes about the server

Measured against `crates/phase-server` v0.59.0:

- One process holds all state (sessions, lobby, SQLite `games.db`), so
  `replicas` is hard-coded to 1 and the Deployment uses `Recreate`.
- `card-data.json` (~100 MiB) is required even in lobby-only mode. A release
  build (`PHASE_CHANNEL=release`) downloads it from `data.phase-rs.dev` on first
  boot; the `startupProbe` allows 10 minutes for that. Images built without a
  channel identity need `server.dataManifestUrl`.
- The server has no TLS, no proxy-header handling and no per-IP limits (only a
  global cap of 200 connections and 30 msgs/s per socket), so those live in
  Traefik middlewares (`traefik.middlewares`). "Per source" is only meaningful
  if Traefik sees real client addresses — see `traefik.middlewares.sourceCriterion`.
- `/admin/*` only exists when `PHASE_ADMIN_TOKEN` is set and is never routed
  through the Ingress; use `kubectl port-forward svc/<release> 9374` (an IP
  allow-list would fail open behind a SNAT'ing load balancer).
- `/p2p-draft-backup` accepts unauthenticated 1 MiB JSON writes that only a
  restart purges; it gets its own Ingress with a body-size cap and a rate limit
  that bounds PVC growth. Size `persistence.size` with that in mind.
- SIGTERM triggers a session flush; open WebSockets are not closed by the server,
  so the pod is killed after `terminationGracePeriodSeconds`.

## Behind Cloudflare

Traefik typically sees a SNAT'd node IP (Service `externalTrafficPolicy: Cluster`),
which makes socket-peer rate limiting meaningless. With `cloudflare.enabled=true`
the limits key on `CF-Connecting-IP` — a header anyone who reaches the origin
directly can forge, and each forged value gets its own bucket, so the chart
refuses to render that way unless the origin is Cloudflare-only: enable
`cloudflare.authenticatedOriginPulls` and turn on *Authenticated Origin Pulls*
for the zone (Traefik then requires Cloudflare's client certificate on the TLS
handshake for this host only), or set `cloudflare.trustHeaderWithoutOriginPulls`
if a firewall or Cloudflare Tunnel already guarantees it. Traefik falls back to default TLS options if another
router serves the same host with different options, so keep all Ingresses for
the host in this chart.

Cloudflare closes idle WebSockets after ~100 s; the client's 5 s application
ping keeps game connections alive.

## Building the image

`ghcr.io/phase-rs/phase-server` is published for linux/amd64 and linux/arm64.
To build your own (the Dockerfile cross-compiles on the build host with zig, so
no emulated cargo; only the runtime stage's `apt-get` runs under QEMU for a
foreign platform):

```bash
docker buildx create --use   # once: multi-platform needs a docker-container builder
docker buildx build --platform linux/arm64 --build-arg PHASE_CHANNEL=release \
  -t <you>/phase-server:v0.59.0 --push .
```

`PHASE_CHANNEL=release` is what lets an empty data volume self-bootstrap.
Pin `image.digest` in your values; `:latest`-style tags resolve stale on some
k3s nodes.

## Values

Every key is documented inline in [`values.yaml`](values.yaml). Single replica,
Recreate strategy and RWO storage are not configurable: they follow from the
server's one-process design, and a rollout is a few seconds of 503s.
