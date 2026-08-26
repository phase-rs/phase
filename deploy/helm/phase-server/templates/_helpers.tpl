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

{{/* Middlewares applied to every public route */}}
{{- define "phase-server.commonMiddlewares" -}}
{{- $list := list -}}
{{- if .Values.traefik.middlewares.enabled -}}
{{- $list = append $list (include "phase-server.crdRef" (dict "ctx" . "suffix" "ratelimit")) -}}
{{- $list = append $list (include "phase-server.crdRef" (dict "ctx" . "suffix" "inflight")) -}}
{{- $list = append $list (include "phase-server.crdRef" (dict "ctx" . "suffix" "headers")) -}}
{{- end -}}
{{- $list = concat $list (default (list) .Values.traefik.middlewares.extra) -}}
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
     rather than by PodMonitor/ServiceMonitor). */}}
{{- define "phase-server.podAnnotations" -}}
{{- $annotations := default (dict) .Values.podAnnotations -}}
{{- if and .Values.metrics.enabled .Values.metrics.annotations -}}
{{- $annotations = merge (dict
      "prometheus.io/scrape" "true"
      "prometheus.io/port" (printf "%v" .Values.metrics.port)
      "prometheus.io/path" .Values.metrics.path) $annotations -}}
{{- end -}}
{{- with $annotations }}
{{- toYaml . }}
{{- end }}
{{- end -}}

{{/* Middleware references for an IngressRoute.

     NOT interchangeable with `phase-server.commonMiddlewares`: that helper emits
     Traefik's *annotation* syntax (`<ns>-<name>@kubernetescrd`), which the CRD
     provider rejects — `@` is not legal in `routes[].middlewares[].name`, and a
     namespace-qualified reference additionally needs `allowCrossNamespace` on
     the Traefik install. A bad reference does not fail loudly: Traefik drops the
     whole route, so the host simply stops answering.

     Same namespace as the IngressRoute, so `namespace:` is left off. */}}
{{- define "phase-server.middlewareRefs" -}}
{{- $fullname := include "phase-server.fullname" . -}}
{{- if .Values.traefik.middlewares.enabled }}
- name: {{ $fullname }}-ratelimit
- name: {{ $fullname }}-inflight
- name: {{ $fullname }}-headers
{{- end }}
{{- range .Values.scaleOut.extraMiddlewareRefs }}
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
{{- if not $scaleOut }}
volumes:
  - name: data
    {{- if .Values.persistence.enabled }}
    persistentVolumeClaim:
      claimName: {{ default (printf "%s-data" (include "phase-server.fullname" .)) .Values.persistence.existingClaim }}
    {{- else }}
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
