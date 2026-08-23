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
- `PUBLIC_URL` is what the server advertises in `ServerHello`, and it is what a
  host's client turns into a `CODE@host` share string. It must be an absolute
  URL with a host (`https://play.example.com`); the server validates it at
  startup and advertises *nothing* if it does not parse, which costs players
  their join links without failing the pod. `server.publicUrl` sets it
  explicitly; otherwise it is derived from `ingress.host`, and rendering fails
  when neither is available rather than guessing a URL.

## Metrics

`metrics.enabled` starts a second listener (`PHASE_METRICS_PORT`) serving
Prometheus text at `/metrics`. It is a separate container port on purpose: the
gauges describe capacity and occupancy, and nothing routes them through the
Ingress.

| Metric | |
|---|---|
| `phase_connections` / `phase_connections_capacity` | open sockets against the cap that returns 503 |
| `phase_games_active` / `phase_games_capacity` | sessions against the cap that refuses `CreateGame` |
| `phase_games_with_connected_humans` | sessions with at least one live player *or spectator* socket |
| `phase_drafts_active` / `phase_drafts_with_connected_humans` | the same pair for server-hosted drafts |
| `phase_replica_ordinal` | this replica's ordinal, when one was set |
| `phase_admission_rejects_total{reason}` | refusals by `connection_limit`, `game_limit`, `origin_not_allowed` |
| `phase_build_info{version,commit,mode}` | build identity, always `1` |

The occupancy gauges count *live sockets*, not map entries — a player who
disconnected leaves their entry behind, and the reconnect grace keeps the
session alive, so "sessions" and "sessions someone is on" are different numbers.

Discovery is a `PodMonitor` (per-pod, so each replica reports its own
occupancy), rendered only when `monitoring.coreos.com/v1` is present so the
chart still installs on a cluster with no prometheus-operator. Set
`metrics.annotations=true` for the `prometheus.io/*` fallback. With
`networkPolicy.enabled`, `metrics.scrapeNamespaceLabels` must name the
scraper's namespace or the target is simply down while the pod stays healthy.

### With kube-prometheus-stack

That chart defaults every selector to "only objects carrying my own release
label":

```yaml
prometheus:
  prometheusSpec:
    podMonitorSelectorNilUsesHelmValues: false
    ruleSelectorNilUsesHelmValues: false
```

Left at the default, Prometheus ignores this chart's `PodMonitor` and
`PrometheusRule` — silently. Nothing errors; `phase:wanted_replicas` simply
never exists, the HPA reports the metric as unavailable, and the deployment
looks healthy throughout. Either set the two keys above, or add the release
label the operator expects via `metrics.podMonitor.labels` and
`autoscaling.prometheusRule.labels`.

Also give Prometheus a `retentionSize`, not just a `retention`. With node-local
storage (k3s `local-path`, hostPath) the volume is the node's root filesystem,
and a retention window sized for a quiet week fills the disk during a busy one —
which evicts pods, this chart's included.

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

## Scaling out

`scaleOut.enabled` replaces the single Deployment with a StatefulSet: one pod,
one PVC and one hostname per ordinal.

**Why not `replicas: N` on the Deployment.** Every process owns its own SQLite
`games.db`, and two processes on one database is destructive rather than merely
racy: the second restores every live game at boot, arms a 120 s reconnect grace
it never had, and its reaper then retires the rows the first process is still
playing — after which the owning process has its snapshots rejected and cannot
write results. `volumeClaimTemplates` is what makes that impossible.

**How a player reaches the right pod.** Each ordinal advertises its own
hostname as `PUBLIC_URL`, so a game created on ordinal 1 produces the share
string `CODE@phase-1.example.com`, and a friend joining by code dials that host
and lands on the pod holding the game. The entry host (`ingress.host`) balances
new arrivals across ready pods with a sticky cookie.

**The sticky cookie is load-bearing, and it is a third-party cookie.** A host's
own game socket is re-opened against the stored entry address after the game
starts, not against the pod it was already talking to, so without the cookie
that socket can land on the wrong pod. Traefik sets it on the 101 response with
`sameSite: none; secure`, which Chrome and Firefox honour and Safari (and
anything blocking third-party cookies) does not — those browsers get a
`(N-1)/N` chance of losing the host's own reconnect. Verify on a two-replica
canary before trusting it:

```bash
curl -i -N -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
  https://phase.example.com/ws | grep -i set-cookie
```

The upstream fix is a client change — derived sockets dialling the
`public_url` from their own `ServerHello` instead of the global server address —
which removes the cookie dependency entirely. Until that lands, treat scale-out
as requiring third-party cookies.

**DNS and certificates.** Ordinal hostnames must sit at the *same* DNS level as
the entry host (`phase-0.example.com`, not `0.phase.example.com`). Behind a CDN
this is not cosmetic: a wildcard edge certificate covers exactly one label, so a
proxied second-level name is served a certificate that does not match it.
`scaleOut.tls` issues one cert-manager `Certificate` covering the entry host and
every ordinal host — IngressRoute is not an Ingress, so cert-manager's
ingress-shim cannot derive it from an annotation. You still need a DNS record
per ordinal (or a wildcard) pointing at the same ingress.

**Middlewares.** `traefik.middlewares.extra` is the Ingress *annotation* syntax
(`<ns>-<name>@kubernetescrd`), which the IngressRoute CRD provider rejects — and
a bad reference makes Traefik drop the whole route rather than fail loudly. With
`scaleOut.enabled` the chart refuses to render if `extra` is set — whatever
`traefik.middlewares.enabled` says, because the value is dropped either way —
so list extras under `scaleOut.extraMiddlewareRefs` as `{name, namespace}`.

### Migrating an existing single-pod release

The Deployment's claim is `<release>-data`; the StatefulSet wants
`data-<release>-0`. **Before upgrading**, either accept a fresh ordinal 0 (it
re-downloads card data and starts with no saved games) or adopt the existing
volume:

```bash
PV=$(kubectl -n phase get pvc <release>-data -o jsonpath='{.spec.volumeName}')
kubectl patch pv "$PV" -p '{"spec":{"persistentVolumeReclaimPolicy":"Retain"}}'
kubectl -n phase scale deploy/<release> --replicas=0
# `scale` returns immediately. Wait for the pod to be GONE before touching the
# volume: rebinding it while the old process still has games.db open is exactly
# the two-writers case the per-ordinal claims exist to prevent.
kubectl -n phase wait --for=delete pod -l app.kubernetes.io/name=phase-server --timeout=120s

kubectl patch pv "$PV" --type=json -p='[{"op":"remove","path":"/spec/claimRef"}]'

# Create the claim FIRST, pointing at the PV, then let the bind happen. Setting
# the PV's claimRef to a claim that does not exist yet moves it to `Released`,
# and a Released PV will not bind to anything.
kubectl -n phase apply -f - <<YAML
apiVersion: v1
kind: PersistentVolumeClaim
metadata: {name: data-<release>-0, namespace: phase}
spec:
  accessModes: [ReadWriteOnce]
  storageClassName: <same as before>
  volumeName: $PV
  resources: {requests: {storage: <same as before>}}
YAML
```

The old `<release>-data` claim is left behind in `Lost` — keep it until ordinal 0
is up on the adopted data, then delete it.

The chart marks the old claim `helm.sh/resource-policy: keep`, so the upgrade
itself will not delete it — but a `Delete` reclaim policy on the PV still will
once the claim goes, which is why the `Retain` patch comes first. Helm reads that
annotation from the **live** object, so a release installed before chart 0.2.0
needs it applied by hand first:

```bash
kubectl -n phase annotate pvc <release>-data helm.sh/resource-policy=keep --overwrite
```

**The TLS secret changes owner.** On the Ingress path cert-manager's ingress-shim
creates a Certificate named after the *secret*; this chart creates one named after
the *release*, and both want the same secret. cert-manager will not overwrite a
secret whose `cert-manager.io/certificate-name` annotation names a different
Certificate — it reports `IncorrectCertificate` and then does nothing: no
CertificateRequest, no Events. The ordinal hosts stay on the old single-SAN
certificate and an edge proxy answers 526 while the entry host keeps working, which
looks like a DNS problem and is not. Hand the secret over once:

```bash
kubectl -n phase annotate secret <release>-tls \
  cert-manager.io/certificate-name=<release> --overwrite
```

## Autoscaling

Turning autoscaling on without the prometheus-operator CRDs is a render-time
error, not a silent one. The HPA's only source of `phase:wanted_replicas` is the
`PrometheusRule`, which needs `monitoring.coreos.com/v1`; installing the HPA
without it would leave it at `FailedGetExternalMetric` forever, so the chart
refuses to render that combination. Install the operator (and prometheus-adapter)
before turning autoscaling on — or, if you produce the recording rule yourself,
set `autoscaling.prometheusRule.enabled=false` and supply it externally (see
`examples/prometheus-adapter-values.yaml`); that path needs no operator at all.

`autoscaling.enabled` (which requires `scaleOut.enabled`) adds a
`PrometheusRule` and an HPA. The **policy lives in the recording rule**, not in
the HPA, because the binding constraint cannot be written as a utilisation
target: a StatefulSet always removes its *highest* ordinal, so scaling in is
only safe when that particular ordinal has nobody on it. The rule takes the
maximum of three terms —

| term | meaning |
|---|---|
| `source="games"` | games packed to `targetUtilization` of a replica's capacity |
| `source="connections"` | the same against the socket cap, which binds first for multiplayer tables |
| `source="occupied_floor"` | highest ordinal still holding a human, plus one |

— clamps it to `[minReplicas, scaleOut.replicaMax]`, and records it as
`phase:wanted_replicas`. The HPA then reads that through prometheus-adapter as
an **External** metric with `target.type: AverageValue, averageValue: "1"`.
`AverageValue` is required: the `Value` path multiplies by the current replica
count, so a metric that already *is* the desired count would compound.

Requires prometheus-operator (for the `PrometheusRule` and `PodMonitor`) and
prometheus-adapter — see
[`examples/prometheus-adapter-values.yaml`](examples/prometheus-adapter-values.yaml).
Only one phase-server release per namespace: the rule aggregates by namespace.

Two things worth knowing before reading the graph:

- The HPA acts only outside its ~10% tolerance band, so treat
  `phase:wanted_replicas` as authoritative for real moves, not for exact
  equality at every instant.
- "Occupied" means *a socket task is alive*. The server sends no keepalive and
  applies no read timeout, so a half-open TCP connection keeps its ordinal
  pinned until the proxy tears it down. Scale-in is deliberately conservative
  here: a pod that is killed preserves its games on its PVC for 24 h and
  restores them if the ordinal returns, whereas a pod held drained loses
  disconnected players' games to the 120 s reaper.

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
