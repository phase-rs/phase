{{- define "phase-server.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "phase-server.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "phase-server.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{ include "phase-server.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "phase-server.selectorLabels" -}}
app.kubernetes.io/name: {{ include "phase-server.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "phase-server.image" -}}
{{- $tag := default (printf "v%s" .Chart.AppVersion) .Values.image.tag -}}
{{- if .Values.image.digest -}}
{{- printf "%s:%s@%s" .Values.image.repository $tag .Values.image.digest -}}
{{- else -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end -}}
{{- end -}}

{{- define "phase-server.logsImage" -}}
{{- $img := .Values.logging.server.image -}}
{{- if $img.digest -}}
{{- printf "%s:%s@%s" $img.repository $img.tag $img.digest -}}
{{- else -}}
{{- printf "%s:%s" $img.repository $img.tag -}}
{{- end -}}
{{- end -}}

{{/* The SPA image. Its tag falls back through image.tag before Chart.AppVersion,
     so it resolves to whatever the server image resolved to unless it is set
     explicitly. That is the version-coupling guarantee: the SPA and the server
     in one pod must stay within one protocol-version step of each other, and
     chaining the fallback through image.tag is what stops `--set image.tag=...`
     from silently leaving the SPA behind on appVersion.

     Which of the two forms below renders is decided by validateWebImage, not
     here: exactly one of `digest` and `followServerTag` is set by the time this
     runs, so the digest branch is the pinned case and the bare-tag branch is
     the affirmed-mutable one. */}}
{{- define "phase-server.webImage" -}}
{{- $img := .Values.web.image -}}
{{- $tag := $img.tag | default .Values.image.tag | default (printf "v%s" .Chart.AppVersion) -}}
{{- if $img.digest -}}
{{- printf "%s:%s@%s" $img.repository $tag $img.digest -}}
{{- else -}}
{{- printf "%s:%s" $img.repository $tag -}}
{{- end -}}
{{- end -}}

{{/* Ports sharing the pod's network namespace must all differ; the loser of a
     collision gets "address in use" at container start, which surfaces as a
     crashloop rather than as a config error. Same class as the metrics/service
     check further down, kept here because the web listener has three siblings
     to clear rather than one. */}}
{{- /* Reject a default server address the client will silently refuse.

     The client validates the value it reads from /config.js and ignores a
     malformed one, falling back to the bundle's build-time default — which in a
     generic image is the public lobby. So a typo here does not break the site,
     it quietly points every new player at someone else's server. Failing the
     render is the only place an operator finds out.

     The rules mirror `parseWebSocketUrl`, which is what the client applies: a
     ws:// or wss:// scheme with a host, and no fragment. The fragment rule tests
     for "#" anywhere rather than a trailing component because the WebSocket
     constructor throws on a bare trailing "#" too. Keep these in step with that
     function; they are one contract expressed on both sides.

     The host is matched as one of three things rather than as "a run of
     characters that are not delimiters": a bracketed IPv6 literal (RFC 4291,
     IPv4-mapped forms included), a dotted-quad IPv4 literal, or a DNS name
     whose final label starts with a letter. That last condition is what keeps
     the two branches apart. URL parsing decides a host is IPv4 by looking at
     its final label, so `999.999.999.999` and `1.2.3.4.5` are IPv4 attempts
     that fail rather than hostnames that succeed, and `0x7f` is a number, not
     a name. A final label starting with a letter cannot be read as either.

     The chart is deliberately stricter here than the parser, which also
     accepts numeric shorthands: a bare integer (`2130706433`), a partial quad
     (`127.1`), and hex or octal octets all resolve to real addresses. None is
     a thing an operator means to type into a server address, and every one of
     them reads as a typo, so the chart refuses them. The rule is only ever
     safe in this direction: the chart must never accept what the parser
     rejects, and may refuse what the parser would have taken.

     The shape rule is anchored at both ends and forbids whitespace anywhere,
     which is deliberately a little stricter than the client. WHATWG URL parsing
     does not merely reject whitespace — it throws on an embedded space, but
     SILENTLY STRIPS a tab or newline, so "wss://host<TAB>name" parses to the
     host "hostname". A literally-equivalent rule would therefore accept a value
     that sends players to a host the operator never typed, which is the same
     class of failure this guard exists to prevent, just quieter. Reject the lot
     and say so. */}}
{{- define "phase-server.validateDefaultServerUrl" -}}
{{- $url := .Values.web.defaultMultiplayerServerUrl -}}
{{- if $url -}}
{{- $re := `^wss?://(\[(([0-9A-Fa-f]{1,4}:){7}[0-9A-Fa-f]{1,4}|([0-9A-Fa-f]{1,4}:){1,7}:|([0-9A-Fa-f]{1,4}:){1,6}:[0-9A-Fa-f]{1,4}|([0-9A-Fa-f]{1,4}:){1,5}(:[0-9A-Fa-f]{1,4}){1,2}|([0-9A-Fa-f]{1,4}:){1,4}(:[0-9A-Fa-f]{1,4}){1,3}|([0-9A-Fa-f]{1,4}:){1,3}(:[0-9A-Fa-f]{1,4}){1,4}|([0-9A-Fa-f]{1,4}:){1,2}(:[0-9A-Fa-f]{1,4}){1,5}|[0-9A-Fa-f]{1,4}:(:[0-9A-Fa-f]{1,4}){1,6}|:((:[0-9A-Fa-f]{1,4}){1,7}|:)|::([Ff]{4}(:0{1,4})?:)?((25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])|([0-9A-Fa-f]{1,4}:){1,4}:((25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9]))\]|((25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])|([A-Za-z0-9_-]+\.)*[A-Za-z][A-Za-z0-9_-]*)(:(6553[0-5]|655[0-2][0-9]|65[0-4][0-9]{2}|6[0-4][0-9]{3}|[1-5][0-9]{4}|[0-9]{1,4}))?([/?][^\s#]*)?$` -}}
{{- if not (regexMatch $re $url) -}}
{{- fail (printf "web.defaultMultiplayerServerUrl is %q, which is not a ws:// or wss:// address with a well-formed host. It must be a hostname or a bracketed IPv6 literal, optionally followed by a port in 0-65535, with no whitespace anywhere. The client ignores an address it cannot parse and falls back to this build's default server, so the deployment would come up pointing players somewhere you did not choose." $url) -}}
{{- end -}}
{{- if contains "#" $url -}}
{{- fail (printf "web.defaultMultiplayerServerUrl is %q, and a WebSocket address may not carry a fragment — the browser's WebSocket constructor rejects one outright. Drop everything from the \"#\" onwards." $url) -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- /* Require an immutable image reference unless mutability is affirmed.

     The SPA is a sidecar in the pod that serves /ws, so a tag that moves under
     the deployment does not merely restyle the site — a bad or missing pull
     takes the game server down with it. Defaulting to a mutable tag would make
     `web.enabled: true` quietly widen the blast radius of every registry-side
     change, so the default is a digest and mutability has to be asked for by
     name.

     The two settings are mutually exclusive rather than merely redundant. A
     digest wins over a tag in an image reference, so `followServerTag: true`
     alongside a digest renders `repo:v<new>@sha256:<old>` — a reference whose
     tag reads current and whose bytes are whatever was pinned. That is worse
     than either choice made alone, because it looks like the tracking that was
     asked for while behaving as the pin that was not. An explicit
     `web.image.tag` is refused with tracking for the same reason: it is the
     tag that would be followed, so setting both means the SPA follows a tag
     the server does not use.

     Tracking also requires `image.tag` to be set, because otherwise there is
     nothing being tracked: the SPA tag falls back to `v<Chart.AppVersion>`, a
     constant this chart carries rather than the release a deployment runs, and
     the appVersion is not bumped at release. For the server image that
     fallback names a tag that exists; for the SPA it names one that never
     will, since the SPA is published only from the release its job first runs
     on. Enforcing it here turns that into a render failure rather than an
     ImagePullBackOff on the pod that also serves /ws.

     Retire this rule if the tag fallback ever becomes release-tracking — put
     `appVersion` in the release replacements, or give the chart a tracking
     authority that is not a constant. The rule exists because the fallback is
     a constant, not because tracking needs a tag spelled out twice. */}}
{{- define "phase-server.validateWebImage" -}}
{{- $img := .Values.web.image -}}
{{- if and $img.digest $img.followServerTag -}}
{{- fail (printf "web.image sets both digest (%q) and followServerTag: true, which contradict each other. A digest wins over a tag, so this would render %s:<server tag>@%s — a reference naming one version and running another. Pick one: keep the digest to pin the SPA, or drop it to track the server's tag." $img.digest $img.repository $img.digest) -}}
{{- end -}}
{{- if and $img.tag $img.followServerTag -}}
{{- fail (printf "web.image sets both tag (%q) and followServerTag: true, which contradict each other — followServerTag means the SPA uses the server's tag, and an explicit tag is the thing it would otherwise follow. Pick one: keep the tag (with a digest, since a tag alone is mutable), or drop it to track the server." $img.tag) -}}
{{- end -}}
{{- if and $img.followServerTag (not .Values.image.tag) -}}
{{- fail (printf "web.image.followServerTag is true but image.tag is empty, so there is no server tag to follow. The SPA tag would fall back to \"v%s\" from the chart's appVersion, which is a constant this chart carries rather than the release the deployment is running, and no %s image is published for releases older than the job that publishes it. Set image.tag to the release you are deploying, or pin web.image.digest instead." .Chart.AppVersion $img.repository) -}}
{{- end -}}
{{- if and (not $img.digest) (not $img.followServerTag) -}}
{{- fail (printf "web.enabled is true but web.image.digest is empty, so %s would be pulled by a mutable tag. The SPA shares a pod with the game server, so a tag that moves under you takes /ws down with the site. Set web.image.digest to a sha256:... reference, or, if something bumps image.tag for you and you want the SPA to move with it, affirm that with web.image.followServerTag: true." $img.repository) -}}
{{- end -}}
{{- end -}}

{{- define "phase-server.validateWebPort" -}}
{{- $web := int .Values.web.server.port -}}
{{- if le $web 1024 -}}
{{- fail (printf "web.server.port is %d, but the SPA sidecar image runs unprivileged (uid %v) and cannot bind a port below 1025." $web .Values.web.server.runAsUser) -}}
{{- end -}}
{{- if eq $web (int .Values.service.port) -}}
{{- fail (printf "web.server.port (%d) must differ from service.port: they are two listeners in one pod, and the loser gets \"address in use\"." $web) -}}
{{- end -}}
{{- if and .Values.metrics.enabled (eq $web (int .Values.metrics.port)) -}}
{{- fail (printf "web.server.port (%d) must differ from metrics.port: they are two listeners in one pod, and the loser gets \"address in use\"." $web) -}}
{{- end -}}
{{- if and .Values.logging.enabled (eq $web (int .Values.logging.server.port)) -}}
{{- fail (printf "web.server.port (%d) must differ from logging.server.port: they are two listeners in one pod, and the loser gets \"address in use\"." $web) -}}
{{- end -}}
{{- end -}}

{{/* PUBLIC_URL is what the server advertises to clients, so it is never
     guessed. Deriving it from ingress.host is only sound when that host is
     actually serving: with the ingress off it yields the values.yaml
     placeholder, and with an empty host it yields "https://", which the server
     warns on and discards (crates/phase-server/src/main.rs). Both are silent
     misconfigurations, so fail rendering instead. */}}
{{- define "phase-server.publicUrl" -}}
{{- if .Values.server.publicUrl -}}
{{- .Values.server.publicUrl -}}
{{- else if and .Values.ingress.enabled .Values.ingress.host -}}
{{- printf "https://%s" .Values.ingress.host -}}
{{- else -}}
{{- fail "server.publicUrl is required here: it is the URL the server advertises to clients, and there is no ingress.host to derive it from (ingress.enabled is false, or ingress.host is empty). Set server.publicUrl, or enable the ingress with a real host." -}}
{{- end -}}
{{- end -}}

{{- define "phase-server.tlsSecretName" -}}
{{- default (printf "%s-tls" (include "phase-server.fullname" .)) .Values.ingress.tls.secretName -}}
{{- end -}}

{{/* "<ns>-<fullname>-<suffix>@kubernetescrd" — Traefik's name for a Middleware/TLSOption CRD */}}
{{- define "phase-server.crdRef" -}}
{{- printf "%s-%s-%s@kubernetescrd" .ctx.Release.Namespace (include "phase-server.fullname" .ctx) .suffix -}}
{{- end -}}

{{/* Middlewares applied to every public route, in Traefik's Ingress
     *annotation* syntax (<ns>-<name>@kubernetescrd).

     Takes dict "ctx" $ "excludeCompress" true|false. Pass true for the /ws
     route: this mirrors the split `build_router` makes in
     crates/phase-server/src/main.rs, which keeps `CompressionLayer` off
     `/ws`'s sub-router because a WebSocket upgrade's 101 response carries no
     body for it to compress. Traefik's compress middleware is documented to
     no-op on such a response too, but running it there is still pure
     overhead (buffering/negotiation work with nothing to show for it), so
     both layers of this stack make the same exclusion for the same reason. */}}
{{- define "phase-server.commonMiddlewares" -}}
{{- $list := list -}}
{{- if .ctx.Values.traefik.middlewares.enabled -}}
{{- /* excludeRateLimit is for the SPA route: the ratelimit/inflight pair is
     sized for API calls, and one cold page load is tens of asset requests, so
     sharing that bucket would throttle the site against the game server's own
     budget. Its own limits live under web.rateLimit. */}}
{{- if not .excludeRateLimit -}}
{{- $list = append $list (include "phase-server.crdRef" (dict "ctx" .ctx "suffix" "ratelimit")) -}}
{{- $list = append $list (include "phase-server.crdRef" (dict "ctx" .ctx "suffix" "inflight")) -}}
{{- end -}}
{{- if and .excludeRateLimit .ctx.Values.web.rateLimit.enabled -}}
{{- $list = append $list (include "phase-server.crdRef" (dict "ctx" .ctx "suffix" "web-ratelimit")) -}}
{{- end -}}
{{- $list = append $list (include "phase-server.crdRef" (dict "ctx" .ctx "suffix" "headers")) -}}
{{- if not .excludeCompress -}}
{{- $list = append $list (include "phase-server.crdRef" (dict "ctx" .ctx "suffix" "compress")) -}}
{{- end -}}
{{- end -}}
{{- $list = concat $list (default (list) .ctx.Values.traefik.middlewares.extra) -}}
{{- join "," $list -}}
{{- end -}}

{{/* Traefik source criterion for the per-source middlewares */}}
{{- define "phase-server.sourceCriterion" -}}
{{- if .Values.cloudflare.enabled -}}
{{- if not (or .Values.cloudflare.authenticatedOriginPulls.enabled .Values.cloudflare.trustHeaderWithoutOriginPulls) -}}
{{- fail "cloudflare.enabled keys rate limits on CF-Connecting-IP, which anyone reaching the origin directly can forge (each forged value gets its own bucket). Enable cloudflare.authenticatedOriginPulls, or set cloudflare.trustHeaderWithoutOriginPulls=true if a firewall/Tunnel already restricts the origin to Cloudflare." -}}
{{- end -}}
requestHeaderName: CF-Connecting-IP
{{- else -}}
{{- toYaml .Values.traefik.middlewares.sourceCriterion -}}
{{- end -}}
{{- end -}}

{{- define "phase-server.ingressAnnotations" -}}
{{- with .Values.ingress.annotations }}
{{- toYaml . }}
{{ end -}}
{{- if .Values.cloudflare.authenticatedOriginPulls.enabled }}
traefik.ingress.kubernetes.io/router.tls.options: {{ include "phase-server.crdRef" (dict "ctx" . "suffix" "cf-origin-pull") | quote }}
{{- end }}
{{- end -}}

{{/* Pod annotations: the operator's own, plus the prometheus.io/* trio when
     metrics.annotations is set (for scrapers that discover by annotation
     rather than by PodMonitor/ServiceMonitor), plus a checksum of the logs
     sidecar's ConfigMap so editing it (e.g. logging.server.port) rolls the
     pod — Kubernetes does not restart pods on ConfigMap changes on its own. */}}
{{- define "phase-server.podAnnotations" -}}
{{- $annotations := default (dict) .Values.podAnnotations -}}
{{- if and .Values.metrics.enabled .Values.metrics.annotations -}}
{{- $annotations = merge (dict
      "prometheus.io/scrape" "true"
      "prometheus.io/port" (printf "%v" .Values.metrics.port)
      "prometheus.io/path" .Values.metrics.path) $annotations -}}
{{- end -}}
{{- if .Values.logging.enabled -}}
{{- $annotations = merge (dict
      "checksum/logs-config" (include (print $.Template.BasePath "/logs-configmap.yaml") . | sha256sum)) $annotations -}}
{{- end -}}
{{- if .Values.web.enabled -}}
{{- $annotations = merge (dict
      "checksum/web-config" (include (print $.Template.BasePath "/web-configmap.yaml") . | sha256sum)) $annotations -}}
{{- end -}}
{{- with $annotations }}
{{- toYaml . }}
{{- end }}
{{- end -}}

{{/* `logging.dir` as a path relative to PHASE_DATA_DIR, i.e. the
     `subPath:` the `logs` sidecar mounts from the shared `data` volume.
     Enforced (not just documented) because there is no other writable
     volume for logs to land on — a value outside PHASE_DATA_DIR would mount
     an empty, unrelated subtree into the sidecar and it would serve nothing.

     Must be a STRICT, traversal-free descendant, not the PVC root itself:
     `logging.dir: /var/lib/phase-server/` string-prefix-matches but trims to
     an empty subPath, which mounts the *entire* volume — games.db and its
     WAL included — into the read-only autoindexed sidecar. A `.` or `..`
     path segment is rejected for the same reason: the check below is a
     string prefix, not a filesystem walk, so `/var/lib/phase-server/../etc`
     would otherwise also match, and kubelet's subPath join treats a bare
     `.` segment as the volume root too (`logging.dir:
     /var/lib/phase-server/.` trims to subPath `"."`, exactly as exposed as
     the empty-subPath case this guard exists for). */}}
{{- define "phase-server.logSubPath" -}}
{{- $prefix := "/var/lib/phase-server/" -}}
{{- if not (hasPrefix $prefix .Values.logging.dir) -}}
{{- fail (printf "logging.dir %q must be a subdirectory of %s (the data PVC mount point) so the logs sidecar can mount it from the same volume." .Values.logging.dir $prefix) -}}
{{- end -}}
{{- $sub := trimPrefix $prefix .Values.logging.dir -}}
{{- if or (eq $sub "") (regexMatch "(^|/)\\.{1,2}($|/)" $sub) -}}
{{- fail (printf "logging.dir %q must be a strict, traversal-free descendant of %s -- the PVC root itself (or a path containing '..') would mount the whole data volume, games.db included, into the read-only autoindexed logs sidecar, not just logs." .Values.logging.dir $prefix) -}}
{{- end -}}
{{- $sub -}}
{{- end -}}

{{/* Middleware references for an IngressRoute.

     NOT interchangeable with `phase-server.commonMiddlewares`: that helper emits
     Traefik's *annotation* syntax (`<ns>-<name>@kubernetescrd`), which the CRD
     provider rejects — `@` is not legal in `routes[].middlewares[].name`, and a
     namespace-qualified reference additionally needs `allowCrossNamespace` on
     the Traefik install. A bad reference does not fail loudly: Traefik drops the
     whole route, so the host simply stops answering.

     Same namespace as the IngressRoute, so `namespace:` is left off.

     Takes dict "ctx" $ "excludeCompress" true|false — see
     `phase-server.commonMiddlewares` for why the /ws route passes true. */}}
{{- define "phase-server.middlewareRefs" -}}
{{- $fullname := include "phase-server.fullname" .ctx -}}
{{- if .ctx.Values.traefik.middlewares.enabled }}
{{- if not .excludeRateLimit }}
- name: {{ $fullname }}-ratelimit
- name: {{ $fullname }}-inflight
{{- end }}
{{- if and .excludeRateLimit .ctx.Values.web.rateLimit.enabled }}
- name: {{ $fullname }}-web-ratelimit
{{- end }}
- name: {{ $fullname }}-headers
{{- if not .excludeCompress }}
- name: {{ $fullname }}-compress
{{- end }}
{{- end }}
{{- range .ctx.Values.scaleOut.extraMiddlewareRefs }}
- name: {{ .name }}
  {{- with .namespace }}
  namespace: {{ . }}
  {{- end }}
{{- end }}
{{- end -}}

{{/* The resolved per-ordinal hostname template, containing {ordinal} once.

     The default keeps ordinals at the SAME DNS level as the entry host
     (`phase-1.example.com`, not `1.phase.example.com`). A wildcard edge
     certificate covers exactly one label, so a proxied second-level name would
     be served a certificate that does not match it. */}}
{{- define "phase-server.ordinalHostTemplate" -}}
{{- $tmpl := .Values.scaleOut.ordinalHostTemplate -}}
{{- if not $tmpl -}}
{{- $host := required "ingress.host is required for scaleOut: it is the entry hostname the ordinal hostnames are derived from." .Values.ingress.host -}}
{{- if not (contains "." $host) -}}
{{- fail (printf "ingress.host %q has a single label, so no ordinal hostname can be derived from it (the result would be %q, which nothing resolves and no certificate covers). Give ingress.host a domain, or set scaleOut.ordinalHostTemplate explicitly." $host (printf "%s-0." $host)) -}}
{{- end -}}
{{- $parts := splitn "." 2 $host -}}
{{- $tmpl = printf "%s-{ordinal}.%s" $parts._0 $parts._1 -}}
{{- end -}}
{{- if ne (len (splitList "{ordinal}" $tmpl)) 2 -}}
{{- fail (printf "scaleOut.ordinalHostTemplate must contain the literal {ordinal} placeholder exactly once; got %q" $tmpl) -}}
{{- end -}}
{{- $tmpl -}}
{{- end -}}

{{/* Hostname for one ordinal: dict "ctx" $ "ordinal" <n> */}}
{{- define "phase-server.ordinalHost" -}}
{{- $split := splitList "{ordinal}" (include "phase-server.ordinalHostTemplate" .ctx) -}}
{{- printf "%s%v%s" (index $split 0) .ordinal (index $split 1) -}}
{{- end -}}

{{/* Every hostname this release answers on: entry host plus one per ordinal. */}}
{{- define "phase-server.allHosts" -}}
{{- $ctx := . -}}
{{- $hosts := list (required "ingress.host is required for scaleOut." .Values.ingress.host) -}}
{{- range $i := until (int .Values.scaleOut.replicaMax) -}}
{{- $hosts = append $hosts (include "phase-server.ordinalHost" (dict "ctx" $ctx "ordinal" $i)) -}}
{{- end -}}
{{- toYaml $hosts -}}
{{- end -}}

{{/* The pod spec, shared by the Deployment (scaleOut off) and the StatefulSet
     (scaleOut on) so the two cannot drift. The differences are real and few:
     the StatefulSet derives PUBLIC_URL and PHASE_REPLICA_ORDINAL from its pod
     ordinal at start-up, and takes its data volume from volumeClaimTemplates
     instead of a single shared claim. */}}
{{- define "phase-server.podSpec" -}}
{{- $scaleOut := .Values.scaleOut.enabled -}}
serviceAccountName: default
automountServiceAccountToken: false
terminationGracePeriodSeconds: {{ .Values.terminationGracePeriodSeconds }}
securityContext:
  {{- toYaml .Values.podSecurityContext | nindent 2 }}
{{- with .Values.dnsConfig }}
dnsConfig:
  {{- toYaml . | nindent 2 }}
{{- end }}
containers:
  - name: phase-server
    image: {{ include "phase-server.image" . }}
    imagePullPolicy: {{ .Values.image.pullPolicy }}
    # Bypass the image's root entrypoint (mkdir/chown + gosu); the pod
    # securityContext already runs us as the `phase` uid with fsGroup.
    {{- if $scaleOut }}
    # Each ordinal advertises its OWN hostname: a game's share string is
    # CODE@<public_url host>, which is what lets a friend joining by code reach
    # the pod that actually holds the game. `--` is $0 so the chart's flags in
    # `args` arrive as "$@".
    {{- $split := splitList "{ordinal}" (include "phase-server.ordinalHostTemplate" .) }}
    command:
      - /bin/sh
      - -c
      - |
        set -eu
        ordinal="${POD_NAME##*-}"
        case "$ordinal" in
          ''|*[!0-9]*)
            echo "cannot derive a StatefulSet ordinal from POD_NAME=$POD_NAME" >&2
            exit 1
            ;;
        esac
        export PHASE_REPLICA_ORDINAL="$ordinal"
        export PUBLIC_URL="${PHASE_ORDINAL_URL_PREFIX}${ordinal}${PHASE_ORDINAL_URL_SUFFIX}"
        echo "phase-server ordinal ${ordinal}, advertising ${PUBLIC_URL}"
        exec phase-server "$@"
      - --
    {{- else }}
    command: ["phase-server"]
    {{- end }}
    {{- /* Emitted even when empty, matching the pre-scaleOut template byte for
         byte. Under the scaleOut shell wrapper an absent list is still correct:
         `$@` expands to nothing and the exec runs with no extra flags. */}}
    args:
      {{- if .Values.server.allowedOrigin }}
      - --allowed-origin
      - {{ .Values.server.allowedOrigin | quote }}
      {{- end }}
      {{- if .Values.server.noDataDownload }}
      - --no-data-download
      {{- end }}
    securityContext:
      {{- toYaml .Values.securityContext | nindent 6 }}
    env:
      - name: PORT
        value: {{ .Values.service.port | quote }}
      - name: PHASE_DATA_DIR
        value: /var/lib/phase-server
      - name: PHASE_LOBBY_ONLY
        value: {{ .Values.server.lobbyOnly | quote }}
      - name: PHASE_CORS_ORIGIN
        value: {{ .Values.server.corsOrigin | quote }}
      - name: PHASE_LOG_JSON
        value: {{ .Values.server.logJson | quote }}
      {{- if .Values.logging.enabled }}
      - name: PHASE_LOG_DIR
        value: {{ .Values.logging.dir | quote }}
      {{- end }}
      - name: RUST_LOG
        value: {{ .Values.server.rustLog | quote }}
      {{- if $scaleOut }}
      - name: POD_NAME
        valueFrom:
          fieldRef:
            fieldPath: metadata.name
      {{- /* The two halves of the ordinal URL arrive as env values, not spliced
           into the shell above: a value carrying a quote or `$(...)` would
           otherwise land inside a double-quoted string and be parsed as shell.
           Env values are plain YAML, so the shell never parses them. */}}
      {{- $split := splitList "{ordinal}" (include "phase-server.ordinalHostTemplate" .) }}
      - name: PHASE_ORDINAL_URL_PREFIX
        value: {{ printf "%s://%s" (.Values.scaleOut.scheme | default "https") (index $split 0) | quote }}
      - name: PHASE_ORDINAL_URL_SUFFIX
        value: {{ index $split 1 | quote }}
      {{- else }}
      - name: PUBLIC_URL
        value: {{ include "phase-server.publicUrl" . | quote }}
      {{- end }}
      {{- if .Values.metrics.enabled }}
      {{- if eq (int .Values.metrics.port) (int .Values.service.port) }}
      {{- fail "metrics.port must differ from service.port: they are two listeners in one pod, and the loser gets \"address in use\"." }}
      {{- end }}
      - name: PHASE_METRICS_PORT
        value: {{ .Values.metrics.port | quote }}
      {{- end }}
      {{- with .Values.server.maxConnections }}
      - name: PHASE_MAX_CONNECTIONS
        value: {{ . | quote }}
      {{- end }}
      {{- with .Values.server.maxGames }}
      - name: PHASE_MAX_GAMES
        value: {{ . | quote }}
      {{- end }}
      {{- with .Values.server.dataManifestUrl }}
      - name: PHASE_DATA_MANIFEST_URL
        value: {{ . | quote }}
      {{- end }}
      {{- with .Values.server.adminTokenSecret }}
      - name: PHASE_ADMIN_TOKEN
        valueFrom:
          secretKeyRef:
            name: {{ . }}
            key: {{ $.Values.server.adminTokenSecretKey }}
      {{- end }}
      {{- with .Values.server.extraEnv }}
      {{- toYaml . | nindent 6 }}
      {{- end }}
    ports:
      - name: http
        containerPort: {{ .Values.service.port }}
        protocol: TCP
      {{- if .Values.metrics.enabled }}
      - name: metrics
        containerPort: {{ .Values.metrics.port }}
        protocol: TCP
      {{- end }}
    startupProbe:
      httpGet:
        path: /health
        port: http
      periodSeconds: {{ .Values.startupProbe.periodSeconds }}
      failureThreshold: {{ .Values.startupProbe.failureThreshold }}
    readinessProbe:
      httpGet:
        path: /health
        port: http
      periodSeconds: 10
    livenessProbe:
      httpGet:
        path: /health
        port: http
      periodSeconds: 30
    resources:
      {{- toYaml .Values.resources | nindent 6 }}
    volumeMounts:
      - name: data
        mountPath: /var/lib/phase-server
  {{- if .Values.logging.enabled }}
  - name: logs
    image: {{ include "phase-server.logsImage" . }}
    imagePullPolicy: {{ .Values.image.pullPolicy }}
    # Read-only static file server for `logging.dir`, diagnosis only — never
    # write access, and only the logs subtree of `data` (not games.db).
    securityContext:
      readOnlyRootFilesystem: true
      allowPrivilegeEscalation: false
      runAsNonRoot: true
      runAsUser: {{ .Values.logging.server.runAsUser }}
      runAsGroup: {{ .Values.logging.server.runAsGroup }}
      capabilities:
        drop: ["ALL"]
    ports:
      - name: logs
        containerPort: {{ .Values.logging.server.port }}
        protocol: TCP
    resources:
      {{- toYaml .Values.logging.server.resources | nindent 6 }}
    volumeMounts:
      - name: data
        mountPath: {{ .Values.logging.dir }}
        subPath: {{ include "phase-server.logSubPath" . }}
        readOnly: true
      - name: logs-conf
        mountPath: /etc/nginx/nginx.conf
        subPath: nginx.conf
        readOnly: true
      - name: logs-tmp
        mountPath: /tmp
  {{- end }}
  {{- if .Values.web.enabled }}
  {{- include "phase-server.validateWebPort" . }}
  {{- include "phase-server.validateWebImage" . }}
  {{- include "phase-server.validateDefaultServerUrl" . }}
  - name: web
    image: {{ include "phase-server.webImage" . }}
    imagePullPolicy: {{ .Values.image.pullPolicy }}
    # Serves the prebuilt SPA baked into the image, with nginx.conf and
    # config.js coming from the ConfigMap instead. Public, unlike the logs
    # sidecar — see web.enabled in values.yaml for the availability coupling
    # this introduces.
    securityContext:
      readOnlyRootFilesystem: true
      allowPrivilegeEscalation: false
      runAsNonRoot: true
      runAsUser: {{ .Values.web.server.runAsUser }}
      runAsGroup: {{ .Values.web.server.runAsGroup }}
      capabilities:
        drop: ["ALL"]
    ports:
      - name: web
        containerPort: {{ .Values.web.server.port }}
        protocol: TCP
    readinessProbe:
      httpGet:
        path: /index.html
        port: web
      periodSeconds: 30
    resources:
      {{- toYaml .Values.web.server.resources | nindent 6 }}
    volumeMounts:
      - name: web-conf
        mountPath: /etc/nginx/nginx.conf
        subPath: nginx.conf
        readOnly: true
      # Whole-directory mount, deliberately: nginx `root`s here for /config.js,
      # so nothing is layered over the image's own copy of that path.
      - name: web-conf
        mountPath: /etc/phase-web
        readOnly: true
      - name: web-tmp
        mountPath: /tmp
  {{- end }}
{{- if or (not $scaleOut) .Values.logging.enabled .Values.web.enabled }}
volumes:
{{- if not $scaleOut }}
  - name: data
    {{- if .Values.persistence.enabled }}
    persistentVolumeClaim:
      claimName: {{ default (printf "%s-data" (include "phase-server.fullname" .)) .Values.persistence.existingClaim }}
    {{- else }}
    emptyDir: {}
    {{- end }}
{{- end }}
{{- if .Values.logging.enabled }}
  - name: logs-conf
    configMap:
      name: {{ include "phase-server.fullname" . }}-logs-conf
  - name: logs-tmp
    emptyDir: {}
{{- end }}
{{- if .Values.web.enabled }}
  - name: web-conf
    configMap:
      name: {{ include "phase-server.fullname" . }}-web-conf
  - name: web-tmp
    emptyDir: {}
{{- end }}
{{- end }}
{{- with .Values.nodeSelector }}
nodeSelector:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.affinity }}
affinity:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.tolerations }}
tolerations:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end -}}

{{/*
Whether each monitor will actually render: the value asks for it AND the cluster
can hold the kind.

Single authority on purpose. `prometheusrule.yaml` fails the render unless a
scrape target exists, and testing only the value there let a cluster with the
PrometheusRule CRD but neither monitor CRD render the rule and the HPA with
nothing scraping the raw gauges. Empty string is false, so callers can use
`if (include ...)`.
*/}}
{{- define "phase-server.podMonitorRenders" -}}
{{- if and .Values.metrics.enabled .Values.metrics.podMonitor.enabled (.Capabilities.APIVersions.Has "monitoring.coreos.com/v1/PodMonitor") -}}
true
{{- end -}}
{{- end -}}

{{- define "phase-server.serviceMonitorRenders" -}}
{{- if and .Values.metrics.enabled .Values.metrics.serviceMonitor.enabled (.Capabilities.APIVersions.Has "monitoring.coreos.com/v1/ServiceMonitor") -}}
true
{{- end -}}
{{- end -}}
