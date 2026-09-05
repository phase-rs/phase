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

Players enter `wss://phase.example.com/ws` in the client's Server picker, or you can
serve them the client too — see [Serving the web client](#serving-the-web-client).

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

## Game logs

`logging.enabled` sets `PHASE_LOG_DIR` to `logging.dir` (default
`/var/lib/phase-server/logs`, a subdirectory of the existing `data` PVC — no
second or shared volume needed) and adds a `logs` sidecar (a small
`nginxinc/nginx-unprivileged` static file server, autoindex on) that mounts
just that subdirectory, read-only, from the same volume: it can list and serve
log files but never touch `games.db`. The server writes the main log
(`phase-server.log`) and one file pair per game (`games/<code>.*`) there; the
per-game format depends on the running image — commits before `e2e5f0ae8`
write flat text `games/<code>.log`, that commit and later write JSON-Lines
`games/<code>.session.jsonl` + `games/<code>.events.jsonl` instead.

Like `/admin`, this is never routed through the Ingress and has no
NetworkPolicy ingress rule. Unlike `/admin`, that policy isn't the only thing
standing between it and other pods: the sidecar binds `127.0.0.1` only, so
the Service/pod-IP path is refused outright even with `networkPolicy.enabled:
false` or a CNI that doesn't enforce NetworkPolicy at all. It's reachable
only from an operator with cluster access, via `kubectl port-forward` (which
tunnels into the pod's own network namespace, so the loopback bind doesn't
block it):

```bash
kubectl -n <namespace> port-forward svc/<release>[-<ordinal>] 8080:<logging.server.port>
curl http://127.0.0.1:8080/games/
```

Under `scaleOut`, logs are per-replica (each ordinal owns its own `data` PVC),
so port-forward the specific ordinal's Service (`<release>-<ordinal>`) — same
as reaching that ordinal's games.

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

## Announcing to a public directory

`server.announceTo` (`PHASE_ANNOUNCE_TO`) makes the server POST a heartbeat to a
server directory every 60 s so players can find it without a link — for the
public one, `https://lobby.phase-rs.dev/servers/announce`.

Three things have to hold, and none of them fails the pod:

- **`PUBLIC_URL` must be `https://`.** The directory lists `wss://` addresses
  only, and it derives them from the advertised URL. An `http://` or unparseable
  value logs one `error!` at startup — `--announce-to is set but this server
  cannot announce itself` — and the heartbeat never starts.
- **`/info` must be reachable from the public internet.** The directory verifies
  a claim by fetching the announced host's `/info` and comparing mode, server
  version and both protocol versions against it. The chart routes `/info` on
  every host it publishes; a proxy or WAF in front of the cluster that hides it
  makes the announcement unverifiable.
- **Outbound 443 must be open.** Setting `server.announceTo` opens the same
  NetworkPolicy egress rule as `networkPolicy.allowBootstrapEgress`.

A directory that is down or refusing logs a `WARN` per tick and nothing else:
announcing never affects games. `directory refused this announcement` carries
the status, so a rejection is distinguishable from an unreachable directory.

Under `scaleOut.enabled` each ordinal announces its own
`phase-<n>.<domain>` — that is the host a join code resolves to. The entry host
is deliberately not announced: it load-balances across pods, so a player dialling
it would land on an arbitrary one.

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

## Serving the web client

`web.enabled=true` adds an nginx sidecar serving the phase web client from the
same hostname as the server, turning one install into a complete site instead of
an endpoint players need a separate client for.

```bash
helm upgrade phase-server deploy/helm/phase-server -n phase \
  --set web.enabled=true \
  --set web.image.digest=sha256:<the SPA image you deployed> \
  --set web.defaultMultiplayerServerUrl=wss://phase.example.com/ws
```

The digest is required; see [Building the image](#building-the-image) for the
one case where you trade it for `web.image.followServerTag: true` instead.

The site is then at `https://phase.example.com/`, and `/ws`, `/health` and
`/p2p-draft-backup` still reach the server. `/admin` is deliberately not routed
with or without the SPA — it stays operator-only over `kubectl port-forward`, so
a request for it from the public edge lands on the site and 404s there.
Routing rests on longest-prefix matching for the plain Ingress and on Traefik's
default rule-length ordering under `scaleOut`;
[`tests/assert-web-routing.sh`](tests/assert-web-routing.sh) asserts that every
route the server actually mounts stays reachable, reading the router itself so a
new server endpoint cannot be silently swallowed by the site's catch-all.

`web.defaultMultiplayerServerUrl` is what new players' Server picker starts on.
The chart renders it into a `/config.js` the client reads at startup, so a single
generic image points at any deployment with no rebuild. Leave it empty and the
bundle keeps its build-time default (the public lobby). A malformed address is
ignored rather than seeded into every profile.

**Keep the two images on one version.** A client accepts a lobby only within one
protocol version of its own build, and the server advertises its number without
being asked — so a web image two releases from its server yields a site that
loads and then cannot connect. `web.image.tag` defaults to `image.tag`, so
pinning the server pins both; override it only together.

Only `/config.js` differs between deployments, and nginx serves it
`must-revalidate` while the service worker is told never to precache it —
otherwise a returning player's browser would keep answering from the copy baked
into the image and never see the deployment's own.

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

**The SPA image must be immutable, or you must say otherwise.** The SPA shares
the server's pod, so an image that resolves to something unpullable takes `/ws`
and `/health` down with the site. With `web.enabled: true` the chart refuses to
render unless you make one of two choices:

| your situation | set | what you get |
| --- | --- | --- |
| you manage upgrades yourself | `web.image.digest: sha256:…` | the exact bytes you tested, until you change them |
| something bumps `image.tag` for you (release automation, GitOps sync) | `web.image.followServerTag: true` | the SPA moves with the server, staying inside one protocol step by construction |

`followServerTag` requires `image.tag`, and the render fails without it. It makes
the SPA use the server's tag, and with `image.tag` empty that falls back to
`v<Chart.appVersion>` — a constant this chart carries, not the release you are
deploying — for which no `phase-web` image is published at all. Pin a digest
instead if you deploy on chart defaults.

They are mutually exclusive, and so are `followServerTag` and an explicit
`web.image.tag`. A digest wins over a tag in an image reference, so allowing both
would render `phase-web:v0.72.0@sha256:<the v0.71.0 image>` — a reference that
names one version and runs another, which is the version-skew failure above
wearing a correct-looking tag. The chart fails the render and says which one to
drop rather than resolving it silently.

If you pin a digest, **bump it when you bump `image.tag`.** Nothing does it for
you: a digest does not move when the server does, and once the two drift past two
releases the site loads and then cannot connect.

`web.image.repository` defaults to `ghcr.io/phase-rs/phase-web`. The job that
publishes it ships separately from this chart (touching a workflow makes a whole
PR maintainer-only), so until that lands, point the value at your own build —
`web.enabled` is false by default, so nothing resolves the image until you opt
in. It is a static bundle over `nginx-unprivileged`, carrying no nginx.conf of
its own because the chart mounts one, so building it is a client build plus a
copy:

```bash
docker buildx create --use --driver docker-container --bootstrap   # once
IMAGE=<you>/phase-web:v0.59.0 ./scripts/build-selfhost-web.sh --push
```

A `docker-container` builder is required: Docker's default driver cannot build
multi-platform at all, and a two-platform result has nowhere to go locally, so
`--push` is not optional either. The script checks both before it starts rather
than after the client build.

It sets the data-plane URLs, strips the JSONs it just pointed elsewhere, and
builds both architectures. It bundles `client/public/card-data.json` when you have
generated one, which is the pool your own engine parses. Without it the client
reads the shared copy, which tracks upstream's releases rather than your checkout
— and because that copy is not content-addressed, the service worker may serve a
cached one for up to 30 days after upstream updates it. Generate your own if that
matters to you.

## Values

Every key is documented inline in [`values.yaml`](values.yaml). Single replica,
Recreate strategy and RWO storage are not configurable: they follow from the
server's one-process design, and a rollout is a few seconds of 503s.
