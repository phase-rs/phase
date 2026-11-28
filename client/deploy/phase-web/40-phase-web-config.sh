#!/bin/sh
# Renders the deployment's /config.js from the environment when the container
# starts. nginx-unprivileged's /docker-entrypoint.sh runs every executable in
# /docker-entrypoint.d/ before nginx, so a plain `docker run -e ...` points this
# one generic image at its own server, without mounting files or rebuilding.
#
#   PHASE_MULTIPLAYER_SERVER_URL   -> multiplayerServerUrl  (wss://host/ws)
#   PHASE_PREVIEW_SITE_URL         -> previewSiteUrl        (https://preview.host/)
#   PHASE_SUPABASE_URL             -> supabaseUrl           (https://ref.supabase.co)
#   PHASE_SUPABASE_ANON_KEY        -> supabaseAnonKey       (the project's anon key)
#
# The two Supabase variables name one project for cloud sync, so set both or
# neither: the client ignores a lone one and keeps its build-time project. That
# project's auth redirect allow-list must include this site's origin. The anon
# key is public by design; Row-Level Security is the access control.
#
# Every variable is optional. An unset or empty one leaves its key out, as the
# phase-server chart's ConfigMap does, and the client then keeps its build-time
# default; the client also ignores a value it cannot parse. The default
# nginx.conf serves the result from /tmp/phase-web/config.js. The chart mounts
# its own nginx.conf and config.js, so there this file is written but unused.
#
# PHASE_WEB_CONFIG_DIR moves the output, for tests.
set -eu

ME=$(basename "$0")
out_dir=${PHASE_WEB_CONFIG_DIR:-/tmp/phase-web}

entrypoint_log() {
  if [ -z "${NGINX_ENTRYPOINT_QUIET_LOGS:-}" ]; then
    echo "$ME: $*"
  fi
}

# A JavaScript string literal holding $1. The value is the operator's, not the
# visitor's, but a stray quote or backslash must still not end the literal early
# and turn the rest of the file into a syntax error that drops every key.
js_string() {
  printf '"%s"' "$(printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
}

newline='
'
carriage_return=$(printf '\r')

mkdir -p "$out_dir"
tmp="$out_dir/config.js.tmp"
set_keys=
{
  echo "// Runtime configuration for this deployment, served at /config.js and read"
  echo "// before the app bundle. Rendered at container start by $ME"
  echo "// from PHASE_* environment variables: set those, not this file."
  echo "window.__PHASE_CONFIG__ = {"
  # <config.js key> <environment variable>, one pair per line.
  while read -r key var; do
    eval "value=\${$var:-}"
    [ -n "$value" ] || continue
    case $value in
      *"$newline"* | *"$carriage_return"*)
        entrypoint_log "ignoring $var: it contains a line break" >&2
        continue
        ;;
    esac
    # printf, not echo: some sh implementations (dash) expand backslash escapes
    # in echo's arguments, which would undo js_string's escaping.
    printf '  %s: %s,\n' "$key" "$(js_string "$value")"
    set_keys="$set_keys $key"
  done <<EOF
multiplayerServerUrl PHASE_MULTIPLAYER_SERVER_URL
previewSiteUrl PHASE_PREVIEW_SITE_URL
supabaseUrl PHASE_SUPABASE_URL
supabaseAnonKey PHASE_SUPABASE_ANON_KEY
EOF
  echo "};"
} >"$tmp"
mv "$tmp" "$out_dir/config.js"

# A lone Supabase key is written like any other, but the client reads the pair
# or nothing, so a deployment with one would quietly keep syncing with the
# build-time project. Say so where the operator looks. Checked on the keys
# written, so a variable dropped above for a line break counts as unset.
supabase_keys=0
for key in supabaseUrl supabaseAnonKey; do
  case " $set_keys " in
    *" $key "*) supabase_keys=$((supabase_keys + 1)) ;;
  esac
done
if [ "$supabase_keys" -eq 1 ]; then
  entrypoint_log "PHASE_SUPABASE_URL and PHASE_SUPABASE_ANON_KEY name one project: with only one usable, the client ignores both" >&2
fi

if [ -n "$set_keys" ]; then
  entrypoint_log "wrote $out_dir/config.js, setting:$set_keys"
else
  entrypoint_log "wrote $out_dir/config.js with no keys: no PHASE_* variable is set"
fi
