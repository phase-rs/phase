#!/usr/bin/env bash
set -euo pipefail

# Asserts that the phase-web image's own defaults, used when it runs without this
# chart, serve the site the way the chart's ConfigMap does:
#
#   - client/deploy/phase-web/nginx.conf is the chart's nginx.conf rendered with
#     web.server.port=8080, except for the one line that says where /config.js
#     comes from. Comments may differ; directives may not.
#   - 40-phase-web-config.sh, given the same addresses as environment variables,
#     yields a /config.js the client reads exactly as it reads the chart's: the
#     files are evaluated and their window.__PHASE_CONFIG__ objects compared,
#     because comments and layout are free to differ.
#   - The Supabase variables, which have no chart value yet, reach /config.js
#     as a pair, and a lone one is reported at start.
#
# Needs helm and node (both on GitHub's ubuntu runners).

chart_dir=$(cd "$(dirname "$0")/.." && pwd)
repo_root=$(cd "$chart_dir/../../.." && pwd)
image_dir="$repo_root/client/deploy/phase-web"
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

fail() { echo "assert-web-image-defaults: $*" >&2; exit 1; }

command -v node >/dev/null || fail "node is required to evaluate config.js"

# The port nginx-unprivileged exposes, and the one the image's nginx.conf listens on.
image_port=8080
web_digest=sha256:0000000000000000000000000000000000000000000000000000000000000000

render_web_configmap() {
  helm template phase-server "$chart_dir" \
    --show-only templates/web-configmap.yaml \
    --set ingress.host=phase.example.test \
    --set ingress.tls.clusterIssuer=letsencrypt \
    --set web.enabled=true --set web.image.digest=$web_digest \
    --set web.server.port=$image_port \
    "$@"
}

# The literal block `$1: |` of a rendered ConfigMap on stdin, dedented.
configmap_file() {
  awk -v key="$1" '
    $0 == "  " key ": |" { on = 1; next }
    on && /^  [^ ]/ { exit }
    on { sub(/^    /, ""); print }
  '
}

# Directives only: no comment lines, blank lines or trailing whitespace.
directives() {
  sed -e 's/[[:space:]]*$//' -e '/^[[:space:]]*#/d' -e '/^$/d' "$1"
}

# ── nginx.conf ───────────────────────────────────────────────────────────────
render_web_configmap | configmap_file nginx.conf >"$work_dir/chart-nginx.conf"
grep -q "listen 0.0.0.0:$image_port;" "$work_dir/chart-nginx.conf" ||
  fail "could not extract the chart's nginx.conf on port $image_port — the extractor is broken, not the config"

# The one intended difference. Counted on both sides so a second `root` for
# config.js, or a moved one, cannot pass as "the same file".
chart_root='root /etc/phase-web;'
image_root='root /tmp/phase-web;'
[ "$(grep -cF "$chart_root" "$work_dir/chart-nginx.conf")" -eq 1 ] ||
  fail "the chart's nginx.conf no longer has exactly one '$chart_root' — update this test and the image's nginx.conf together"
[ "$(grep -cF "$image_root" "$image_dir/nginx.conf")" -eq 1 ] ||
  fail "client/deploy/phase-web/nginx.conf must serve /config.js from /tmp/phase-web, where 40-phase-web-config.sh writes it"

# ── The image actually uses these files, and they agree on the path ─────────
# Everything below checks files; these lines check that the Dockerfile installs
# them where nginx and its entrypoint look, and that the script writes config.js
# where the nginx.conf serves it from. Either one moving alone would ship an
# image that answers /config.js with a 404 while every other check passed.
dockerfile="$repo_root/deploy/phase-web.Dockerfile"
for copy in \
  "COPY --chmod=0644 deploy/phase-web/nginx.conf /etc/nginx/nginx.conf" \
  "COPY --chmod=0755 deploy/phase-web/40-phase-web-config.sh /docker-entrypoint.d/"; do
  grep -qxF "$copy" "$dockerfile" ||
    fail "deploy/phase-web.Dockerfile no longer has '$copy', so the image would not use the file this test checks"
done
grep -qxF 'out_dir=${PHASE_WEB_CONFIG_DIR:-/tmp/phase-web}' "$image_dir/40-phase-web-config.sh" ||
  fail "40-phase-web-config.sh must write to /tmp/phase-web by default, where the image's nginx.conf serves /config.js from ($image_root)"

directives "$work_dir/chart-nginx.conf" | sed "s|$chart_root|$image_root|" >"$work_dir/chart.directives"
directives "$image_dir/nginx.conf" >"$work_dir/image.directives"
if ! diff -u "$work_dir/chart.directives" "$work_dir/image.directives" >"$work_dir/nginx.diff"; then
  cat "$work_dir/nginx.diff" >&2
  fail "client/deploy/phase-web/nginx.conf differs from the chart's web nginx.conf (above: - chart, + image)"
fi

# ── config.js ────────────────────────────────────────────────────────────────
# window.__PHASE_CONFIG__ after running file $1 the way a page would, as JSON
# with sorted keys; "undefined" if the file does not define it.
evaluate_config() {
  node -e '
    const vm = require("vm");
    const window = {};
    vm.runInNewContext(require("fs").readFileSync(process.argv[1], "utf8"), { window });
    const config = window.__PHASE_CONFIG__;
    console.log(config === undefined ? "undefined"
      : JSON.stringify(config, Object.keys(config).sort()));
  ' "$1"
}

# Runs the entrypoint script with the given VAR=value arguments as its whole
# environment and evaluates what it wrote.
image_config() {
  local out="$work_dir/image-config"
  rm -rf "$out"
  env -i PATH="$PATH" NGINX_ENTRYPOINT_QUIET_LOGS=1 PHASE_WEB_CONFIG_DIR="$out" "$@" \
    sh "$image_dir/40-phase-web-config.sh"
  evaluate_config "$out/config.js"
}

chart_config() {
  render_web_configmap "$@" | configmap_file config.js >"$work_dir/chart-config.js"
  evaluate_config "$work_dir/chart-config.js"
}

server_url=wss://phase.example.test/ws
preview_url=https://preview.example.test/

# Same addresses through each door: chart values on the left, environment on the right.
same_config() {
  local label=$1 chart_args=$2 env_args=$3 chart image
  # Word splitting is the point: each string is a list of arguments.
  # shellcheck disable=SC2086
  chart=$(chart_config $chart_args)
  # shellcheck disable=SC2086
  image=$(image_config $env_args)
  [ "$chart" != undefined ] || fail "$label: the chart's config.js defines no window.__PHASE_CONFIG__"
  [ "$chart" = "$image" ] ||
    fail "$label: the chart's config.js reads as $chart, the image's as $image"
}

same_config "nothing set" "" ""
same_config "server only" \
  "--set web.defaultMultiplayerServerUrl=$server_url" \
  "PHASE_MULTIPLAYER_SERVER_URL=$server_url"
same_config "preview only" \
  "--set web.previewSiteUrl=$preview_url" \
  "PHASE_PREVIEW_SITE_URL=$preview_url"
same_config "both" \
  "--set web.defaultMultiplayerServerUrl=$server_url --set web.previewSiteUrl=$preview_url" \
  "PHASE_MULTIPLAYER_SERVER_URL=$server_url PHASE_PREVIEW_SITE_URL=$preview_url"

# Positive control: the comparison above must be able to see a value at all,
# or "nothing set" and "both" would pass for the same wrong reason.
[ "$(image_config PHASE_MULTIPLAYER_SERVER_URL=$server_url)" = "{\"multiplayerServerUrl\":\"$server_url\"}" ] ||
  fail "the image's config.js does not carry PHASE_MULTIPLAYER_SERVER_URL through"

# Values the chart refuses to render still must not break the file: the client
# ignores an unparseable address, but a syntax error would drop every key.
tricky='wss://phase.example.test/ws"x\y'
expected=$(node -e 'console.log(JSON.stringify({ multiplayerServerUrl: process.argv[1] }))' "$tricky")
[ "$(image_config "PHASE_MULTIPLAYER_SERVER_URL=$tricky")" = "$expected" ] ||
  fail "a quote or backslash in PHASE_MULTIPLAYER_SERVER_URL is not escaped"
[ "$(image_config "PHASE_MULTIPLAYER_SERVER_URL=wss://a.example.test/ws
x" "PHASE_PREVIEW_SITE_URL=$preview_url")" = "{\"previewSiteUrl\":\"$preview_url\"}" ] ||
  fail "a line break in one variable must drop that key only"

# ── Supabase: image only, the chart has no value for these yet ──────────────
supabase_url=https://project-ref.example.test
supabase_key=sb_publishable_example
[ "$(image_config PHASE_SUPABASE_URL=$supabase_url PHASE_SUPABASE_ANON_KEY=$supabase_key)" = \
  "{\"supabaseAnonKey\":\"$supabase_key\",\"supabaseUrl\":\"$supabase_url\"}" ] ||
  fail "the image's config.js does not carry PHASE_SUPABASE_URL and PHASE_SUPABASE_ANON_KEY through"

# What the script says at start, on stderr, for the given VAR=value arguments.
start_warnings() {
  env -i PATH="$PATH" PHASE_WEB_CONFIG_DIR="$work_dir/warn-config" "$@" \
    sh "$image_dir/40-phase-web-config.sh" 2>&1 >/dev/null
}
pair_warning='name one project'
# Positive control first: without it, every "no warning" case below would pass
# against a script that never warns.
for lone in "PHASE_SUPABASE_URL=$supabase_url" "PHASE_SUPABASE_ANON_KEY=$supabase_key"; do
  start_warnings "$lone" | grep -qF "$pair_warning" ||
    fail "with only $lone set, the script does not report that the client ignores it"
done
# A value dropped for its line break leaves the other one alone.
start_warnings "PHASE_SUPABASE_URL=$supabase_url
x" "PHASE_SUPABASE_ANON_KEY=$supabase_key" | grep -qF "$pair_warning" ||
  fail "a Supabase variable dropped for a line break must count as unset in the pair check"
for complete in "" "PHASE_SUPABASE_URL=$supabase_url PHASE_SUPABASE_ANON_KEY=$supabase_key"; do
  # shellcheck disable=SC2086
  ! start_warnings $complete | grep -qF "$pair_warning" ||
    fail "the script reports a lone Supabase variable with '${complete:-nothing}' set"
done

echo "assert-web-image-defaults: PASS"
