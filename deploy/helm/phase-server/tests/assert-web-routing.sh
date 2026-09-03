#!/usr/bin/env bash
set -euo pipefail

# Asserts the routing contract that `web.enabled` depends on: the SPA takes the
# catch-all, and every HTTP endpoint the server actually mounts still reaches the
# server. Renders the chart itself rather than taking pre-rendered files (as
# assert-compression-boundary.sh does) because the server-surface check below
# reads the Rust router, so the script is repo-rooted either way.

chart_dir=$(cd "$(dirname "$0")/.." && pwd)
repo_root=$(cd "$chart_dir/../../.." && pwd)
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

render() {
  local out=$1; shift
  helm template phase-server "$chart_dir" \
    --set ingress.host=phase.example.test \
    --set ingress.tls.clusterIssuer=letsencrypt \
    --set server.adminTokenSecret=phase-admin \
    "$@" > "$out"
}

# A digest is what an operator is steered to set, so the routing renders below
# use one. Without it the chart refuses to render at all, which the image-policy
# cases further down assert directly.
web_digest=sha256:0000000000000000000000000000000000000000000000000000000000000000
render "$work_dir/web.yaml" --set web.enabled=true --set web.image.digest=$web_digest
render "$work_dir/web-scaleout.yaml" --set web.enabled=true --set web.image.digest=$web_digest \
  --set scaleOut.enabled=true
render "$work_dir/noweb.yaml"

extract_doc() {
  awk -v kind="$1" -v name="$2" '
    BEGIN { RS = "---"; ORS = "" }
    $0 ~ "kind: " kind "\\n" && $0 ~ "metadata:\\n  name: " name "\\n" { print }
  ' "$3"
}

fail() { echo "assert-web-routing: $*" >&2; exit 1; }

# `/admin/*` is mounted by the server but deliberately NOT routed at the edge.
# Its bearer guard is authentication, not a reason to move the chart's documented
# network boundary, so it stays reachable only through `kubectl port-forward`.
# Listed here rather than silently skipped: the loop below excludes exactly these
# prefixes from the must-be-routed check, and the negative assertion further down
# requires each of them to be ABSENT from the render even with a token set. A new
# server route still has to be routed or explicitly listed here.
OPERATOR_ONLY="/admin"

# ── The server's real HTTP surface ──────────────────────────────────────────
# Walked from the router rather than recalled, so a route added to the server
# without a matching ingress rule fails here instead of being silently swallowed
# by the SPA catch-all. `/metrics` is deliberately absent: it is mounted on a
# separate listener outside build_router and must not be publicly routed.
# ── This test must actually run when its inputs change ──────────────────────
# The script reads the chart and the axum router. A CI path filter that lists
# only the chart means a router-only change — the very case the routing check
# exists to catch — merges without running it. Rather than rely on remembering
# to keep the two in step, assert it: every repo file this script reads must be
# covered by the workflow's trigger paths.
workflow="$repo_root/.github/workflows/helm-chart.yml"
if [ -f "$workflow" ]; then
  # `|| true`: with no match the pipeline exits non-zero, and under `set -e` the
  # assignment would kill the script before the emptiness check below could
  # report it — a silent exit 1 that looks like a broken script rather than a
  # dead parse.
  trigger_paths=$(sed -n "/^on:/,/^permissions:/p" "$workflow" |
    grep -oE "^ +- '[^']+'" | grep -oE "'[^']+'" | tr -d "'" | sort -u || true)
  [ -n "$trigger_paths" ] ||
    fail "no trigger paths parsed out of $workflow — the coverage check below cannot run"
  covered() {                       # $1 = repo-relative path
    local want=$1 pat
    while IFS= read -r pat; do
      case "$pat" in
        */\*\*) [ "${want#"${pat%/\*\*}"/}" != "$want" ] && return 0 ;;
        *)        [ "$want" = "$pat" ] && return 0 ;;
      esac
    done <<<"$trigger_paths"
    return 1
  }
  # Positive control: a path that is obviously covered must report covered, or
  # the matcher is broken and every check below passes for the wrong reason.
  covered "deploy/helm/phase-server/values.yaml" ||
    fail "trigger-path matcher is broken: it does not match a chart file"
  # Negative control: a matcher that says yes to everything would pass the check
  # above and every check below without testing anything.
  ! covered "README.md" ||
    fail "trigger-path matcher is broken: it matches a path no trigger lists"
  for input in "crates/phase-server/src/main.rs" "deploy/helm/phase-server/tests/assert-web-routing.sh"; do
    covered "$input" ||
      fail "$input is read by this test but is not in the trigger paths of ${workflow#"$repo_root/"} — a change to it would skip this check"
  done
fi

server_src="$repo_root/crates/phase-server/src/main.rs"
prefixes=$(
  { awk '/^fn build_router\(/,/^}/' "$server_src"
    awk '/^fn mount_admin_routes\(/,/^}/' "$server_src"; } |
    tr '\n' ' ' |
    grep -oE '\.route\( *"[^"]+"' |
    grep -oE '"[^"]+"' | tr -d '"' |
    sed 's|\(/[^/]*\).*|\1|' | sort -u
)
# Live instrument: an extractor that silently stopped matching would otherwise
# report an empty surface, and every "is it routed?" check below would pass
# vacuously.
grep -qx '/ws' <<<"$prefixes" || fail "route extractor found no /ws — it is broken, not the chart ($(tr '\n' ' ' <<<"$prefixes"))"
[ "$(wc -l <<<"$prefixes")" -ge 3 ] || fail "route extractor found only $(wc -l <<<"$prefixes") prefixes: $(tr '\n' ' ' <<<"$prefixes")"
echo "server HTTP prefixes: $(tr '\n' ' ' <<<"$prefixes")"

routed_any=0
while IFS= read -r prefix; do
  if grep -qx -- "$prefix" <<<"$OPERATOR_ONLY"; then continue; fi
  grep -q "path: $prefix\$" "$work_dir/web.yaml" ||
    fail "$prefix is mounted by the server but no Ingress routes it — the SPA catch-all would swallow it"
  grep -qF "PathPrefix(\`$prefix\`)" "$work_dir/web-scaleout.yaml" ||
    fail "$prefix is mounted by the server but no IngressRoute rule routes it under scaleOut"
  routed_any=1
done <<<"$prefixes"
# The exclusion list must not be able to empty the positive check.
[ "$routed_any" = "1" ] || fail "no prefix was checked for routing — the exclusion list swallowed the whole surface"

# ── Negative: the operator-only surface must not be published ───────────────
# Both renders above set server.adminTokenSecret, so the admin routes ARE mounted
# in the pod; these assertions are about the edge, not about the feature.
grep -q "PHASE_ADMIN_TOKEN" "$work_dir/web.yaml" ||
  fail "the admin token is not wired into the pod, so the absence checks below would pass vacuously"
while IFS= read -r prefix; do
  for render in web.yaml web-scaleout.yaml noweb.yaml; do
    if grep -q -- "$prefix" "$work_dir/$render"; then
      fail "$prefix appears in the $render render — it must stay port-forward-only (see templates/ingress.yaml)"
    fi
  done
done <<<"$OPERATOR_ONLY"

# ── Plain Ingress topology: SPA on "/", server endpoints on the server port ──
# Ingress matching is longest-prefix, so "/" losing to every endpoint above is
# structural rather than an ordering we have to assert.
web_ing="$work_dir/web-ingress.yaml"
extract_doc Ingress phase-server-web "$work_dir/web.yaml" > "$web_ing"
test -s "$web_ing" || fail "web.enabled rendered no phase-server-web Ingress"
grep -q 'path: /$' "$web_ing" || fail "phase-server-web does not take the / catch-all"
grep -q 'name: web$' "$web_ing" || fail "phase-server-web does not target the web Service port"
for name in phase-server phase-server-ws phase-server-backup; do
  extract_doc Ingress "$name" "$work_dir/web.yaml" | grep -q 'name: http$' ||
    fail "$name stopped targeting the server port with web.enabled"
done

# ── scaleOut IngressRoute: catch-all is the SPA, every longer rule is the server ──
# Traefik sorts routers on an entrypoint by descending rule-string length when no
# `priority:` is set, so "longer than the catch-all" IS the priority contract.
# See templates/ingressroute.yaml for why no explicit priority is ever pinned.
entry="$work_dir/entry.yaml"
extract_doc IngressRoute phase-server "$work_dir/web-scaleout.yaml" > "$entry"
test -s "$entry" || fail "no entry IngressRoute rendered"
if grep -q '^      priority:' "$entry"; then
  fail "entry IngressRoute pins an explicit priority — see templates/ingressroute.yaml"
fi

awk '
  /^    - kind: Rule/ { if (m != "") print m "\t" sticky "\t" port; m=""; sticky="no"; port=""; next }
  /^      match: / { m = substr($0, 14) }
  /^          sticky:/ { sticky = "yes" }
  /^          port: / { port = $2 }
  END { if (m != "") print m "\t" sticky "\t" port }
' "$entry" > "$work_dir/rules.tsv"

catchall=$(awk -F'\t' '$1 !~ / && / { print $1 }' "$work_dir/rules.tsv")
[ -n "$catchall" ] || fail "entry IngressRoute has no catch-all rule"
[ "$(wc -l <<<"$catchall")" -eq 1 ] || fail "entry IngressRoute has more than one catch-all rule: $catchall"

web_port=$(extract_doc Service phase-server "$work_dir/web.yaml" |
  awk '/- name: web$/{found=1} found && /port: /{print $2; exit}')
[ -n "$web_port" ] || fail "could not read the web port out of the rendered Service"

while IFS=$'\t' read -r match sticky port; do
  if [ "$match" = "$catchall" ]; then
    [ "$port" = "$web_port" ] || fail "catch-all targets port $port, not the SPA's $web_port"
    # The SPA and /ws are two load balancers over different server lists sharing
    # one cookie name; a sticky SPA would re-mint it with a value /ws cannot
    # resolve. templates/ingressroute.yaml carries the full reasoning.
    [ "$sticky" = "no" ] || fail "catch-all carries a sticky cookie, which would collide with the /ws balancer's"
    continue
  fi
  [ "${#match}" -gt "${#catchall}" ] ||
    fail "rule \"$match\" is not longer than the catch-all \"$catchall\" — Traefik would not rank it first"
  case "$match" in
    "$catchall"' && PathPrefix(`'*'`)') ;;
    *) fail "rule \"$match\" is not the catch-all plus a PathPrefix suffix" ;;
  esac
  [ "$sticky" = "yes" ] || fail "server rule \"$match\" lost its sticky cookie"
done < "$work_dir/rules.tsv"

# ── The default-server address is validated the way the client validates it ──
# A value the client refuses is worse than a render failure: the site comes up and
# quietly uses the bundle's own default instead of the operator's server. Whitespace
# is rejected outright because URL parsing STRIPS a tab or newline rather than
# failing, which would silently change the host.
url_case() {
  local expect=$1 value=$2 out
  if out=$(helm template phase-server "$chart_dir" --set ingress.host=phase.example.test \
      --set web.enabled=true --set web.image.digest=$web_digest \
      --set-string web.defaultMultiplayerServerUrl="$value" 2>&1 >/dev/null); then
    [ "$expect" = "render" ] || fail "web.defaultMultiplayerServerUrl=$(printf %q "$value") rendered, but the client would refuse it"
  else
    [ "$expect" = "refuse" ] || fail "web.defaultMultiplayerServerUrl=$(printf %q "$value") was refused, but it is a valid address"
  fi
}
url_case render 'wss://play.example.com/ws'
url_case render 'ws://192.168.1.5:9374/ws'
url_case render 'wss://play.example.com/ws?region=eu'
url_case render ''
url_case refuse 'https://play.example.com'
url_case refuse 'play.example.com'
url_case refuse 'wss://'
url_case refuse 'wss://play.example.com bad'
url_case refuse "wss://play.example.com$(printf '\t')bad"
url_case refuse ' wss://play.example.com/ws'
url_case refuse 'wss://play.example.com/ws '
url_case refuse 'wss://play.example.com/ws#lobby'
url_case refuse 'wss://play.example.com/ws#'

# Authority grammar. The chart's accept-set must stay a SUBSET of what
# `parseWebSocketUrl` accepts: anything the chart admits and the client drops is
# a deployment that renders clean and then silently uses the build-time default.
# Verdicts below are the client's, measured with node's WHATWG URL rather than
# recalled — the corpus and the comparison live in client/src/config.
url_case render 'wss://[::1]/ws'
url_case render 'wss://[::1]:9374/ws'
url_case render 'wss://[2001:db8::8a2e:370:7334]/ws'
url_case render 'wss://play.example.com:65535/ws'
url_case render 'wss://play.example.com:0/ws'
url_case refuse 'wss://play.example.com:abc/ws'      # non-numeric port
url_case refuse 'wss://play.example.com:99999/ws'    # port above 65535
url_case refuse 'wss://play.example.com:-1/ws'       # negative port
url_case refuse 'wss://[::1/ws'                      # unclosed bracket
url_case refuse 'wss://[]/ws'                        # empty bracket
url_case refuse 'wss://]::1[/ws'                     # reversed brackets
url_case render 'wss://[::]/ws'
url_case render 'wss://[::ffff:192.168.1.1]/ws'
url_case refuse 'wss://[:::::]/ws'                   # more than one elision
url_case refuse 'wss://[1::2::3]/ws'                 # two elisions, no ":::" substring
url_case refuse 'wss://[1:1:1]/ws'                   # too few groups, no elision
url_case refuse 'wss://[1:2:3:4:5:6:7:8:9]/ws'       # too many groups
url_case refuse 'wss://[gggg::1]/ws'                 # non-hex group
url_case refuse 'wss://:9374/ws'                     # port but no host
url_case refuse 'wss://@/ws'                         # empty authority
url_case refuse 'wss://%00.com/ws'                   # percent-encoding in a host

# Dotted-numeric authorities. URL parsing decides a host is an IPv4 attempt from
# its final label, so these fail to parse rather than resolving as hostnames.
url_case render 'wss://192.168.1.5:9374/ws'
url_case render 'wss://255.255.255.255/ws'
url_case refuse 'wss://999.999.999.999/ws'           # octets out of range
url_case refuse 'wss://256.1.1.1/ws'                 # first octet out of range
url_case refuse 'wss://1.2.3.4.5/ws'                 # five parts
url_case refuse 'wss://0x7f.0.0.1/ws'                # hex octet: a number, not a name

# ── The SPA image must be immutable unless mutability is asked for by name ──
# The SPA is a sidecar in the pod that serves /ws, so a tag that moves under the
# deployment can take the game server down with the site. A digest is therefore
# the default and a mutable tag needs an affirmative opt-in. The two are mutually
# exclusive: a digest wins over a tag, so allowing both would render a reference
# whose tag reads current and whose bytes are whatever was pinned.
image_case() {
  local expect=$1 desc=$2; shift 2
  local out
  if out=$(helm template phase-server "$chart_dir" --set ingress.host=phase.example.test \
      --set web.enabled=true "$@" 2>&1); then
    [ "$expect" = "render" ] || fail "web.image case '$desc' rendered, but the chart should refuse it"
    printf '%s' "$out"
  else
    [ "$expect" = "refuse" ] || fail "web.image case '$desc' was refused, but it is a valid configuration"
    printf ''
  fi
}

# The default is refused: enabling the SPA must not silently add a mutable pull.
image_case refuse 'no digest, no opt-in' >/dev/null
image_case refuse 'tag alone is still mutable' --set web.image.tag=v1.2.3 >/dev/null

# A pinned digest renders, and the digest actually reaches the container.
pinned=$(image_case render 'digest pinned' --set web.image.digest=$web_digest |
  grep -oE "ghcr\.io/phase-rs/phase-web:[^ ]+")
case "$pinned" in
  *"@$web_digest") ;;
  *) fail "a pinned web.image.digest rendered as '$pinned', which does not carry the digest" ;;
esac

# Tracking renders only when deliberately selected, and then it really tracks:
# overriding the server's tag must move the SPA with it, since that coupling is
# the whole reason to accept a mutable tag.
tracked=$(image_case render 'followServerTag' --set web.image.followServerTag=true \
  --set image.tag=v9.9.9 | grep -oE "ghcr\.io/phase-rs/phase-web:[^ ]+")
[ "$tracked" = "ghcr.io/phase-rs/phase-web:v9.9.9" ] ||
  fail "followServerTag rendered '$tracked'; it must follow the server's tag (v9.9.9)"
case "$tracked" in
  *@sha256:*) fail "followServerTag rendered a digest ('$tracked'), so it would not track anything" ;;
esac

# Tracking with nothing to track is refused. This is the configuration an
# operator reaches from chart defaults: the SPA tag would fall back to
# v<Chart.appVersion>, a constant rather than the release being deployed, and no
# SPA image is published for releases older than the job that publishes it. It
# is the only rule that can reject this input, so the case cannot pass for
# another reason.
image_case refuse 'followServerTag with no server tag to follow' \
  --set web.image.followServerTag=true >/dev/null

# Asking for both is refused rather than silently resolved in the digest's
# favour. These carry image.tag so the contradiction is the only defect left —
# without it they would refuse on the missing-server-tag rule and prove nothing
# about mutual exclusivity.
image_case refuse 'digest + followServerTag' --set image.tag=v9.9.9 \
  --set web.image.digest=$web_digest --set web.image.followServerTag=true >/dev/null
image_case refuse 'tag + followServerTag' --set image.tag=v9.9.9 \
  --set web.image.tag=v1.2.3 --set web.image.followServerTag=true >/dev/null

# ── Opt-in stays opt-in ─────────────────────────────────────────────────────
if grep -q 'name: phase-server-web$' "$work_dir/noweb.yaml"; then
  fail "web resources render with web.enabled=false"
fi

echo "assert-web-routing: PASS"
