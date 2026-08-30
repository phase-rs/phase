#!/usr/bin/env bash
# Tests for scripts/lib/pnpm-preflight.sh.
#
# The regression these exist for: the pin lives in client/package.json, but the
# first version of this check ran `pnpm --version` from the repo root. Because
# `packageManager` is directory-scoped, root and client/ legitimately resolve
# DIFFERENT pnpm versions on the same machine (CI measured root 11.24.0 vs
# client 9.15.9). Measuring the root therefore hard-failed setup on an
# environment where `cd client && pnpm install` would have used the pin
# correctly — rejecting a valid machine before installing anything.
#
# So these drive `pnpm_preflight_check` — the DECISION — not the resolver in
# isolation. A test that only asserts "the resolver reads some version" cannot
# catch a resolver reading the version from the wrong directory.
#
# pnpm is stubbed: a `pnpm` on PATH that reports a different version depending
# on its working directory. That is the only way to reproduce the divergence
# without installing two real pnpm majors.
#
# Venue: the Tilt `pnpm-preflight` resource (label 'lint'). NOT GitHub CI —
# enrolling a script gate there needs a `.github/workflows/**` edit, which is a
# hard stop for agent changes; probe-pin is enforced the same way for the same
# reason (docs/probe-pin.md). This gate is therefore local-only.
#
# Run:  bash scripts/lib/pnpm_preflight_tests.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/pnpm-preflight.sh
source "$SCRIPT_DIR/pnpm-preflight.sh"

PASS=0
FAIL=0

fail() { printf '  FAIL: %s\n' "$1" >&2; FAIL=$((FAIL + 1)); }
ok()   { printf '  ok: %s\n' "$1";     PASS=$((PASS + 1)); }

# Build a throwaway repo: a root with NO package.json (as in the real repo), a
# client/ whose package.json carries $1, and a stubbed pnpm reporting $2 at the
# root and $3 anywhere under client/.
make_fixture() {
  local pin="$1" root_version="$2" client_version="$3"
  FIXTURE="$(mktemp -d)"
  mkdir -p "$FIXTURE/client" "$FIXTURE/bin"

  if [ -n "$pin" ]; then
    printf '{\n  "name": "c",\n  "packageManager": "%s"\n}\n' "$pin" > "$FIXTURE/client/package.json"
  else
    printf '{\n  "name": "c"\n}\n' > "$FIXTURE/client/package.json"
  fi

  # The stub is the whole point: it reports by directory, the way corepack and
  # pnpm >= 10 actually behave when they find a packageManager field above cwd.
  cat > "$FIXTURE/bin/pnpm" <<STUB
#!/bin/sh
case "\$PWD" in
  */client|*/client/*) printf '%s\n' '$client_version' ;;
  *)                   printf '%s\n' '$root_version' ;;
esac
STUB
  chmod +x "$FIXTURE/bin/pnpm"
}

# Run pnpm_preflight_check against the fixture, with only the stub on PATH.
check_in_fixture() {
  ( cd "$FIXTURE" && PATH="$FIXTURE/bin:/usr/bin:/bin" \
      bash -c "source '$SCRIPT_DIR/pnpm-preflight.sh'; pnpm_preflight_check client" ) \
    >/dev/null 2>&1
}

expect() {  # $1 = label, $2 = expected exit
  local label="$1" want="$2" got
  check_in_fixture; got=$?
  if [ "$got" = "$want" ]; then ok "$label"; else fail "$label (want exit $want, got $got)"; fi
  rm -rf "$FIXTURE"
}

echo "pnpm-preflight tests"

# THE REGRESSION. Root resolves a newer pnpm, client/ resolves the pin. This is
# a correct environment and must be accepted. The root-measuring version of the
# check returned 1 here, blocking setup on a machine that was fine.
make_fixture "pnpm@9.15.9" "11.24.0" "9.15.9"
expect "divergent root/client resolution is accepted (client honours the pin)" 0

# The mirror image: client/ itself resolves the wrong major. This is the real
# breakage the check exists to stop, and it must still be caught.
make_fixture "pnpm@9.15.9" "9.15.9" "11.24.0"
expect "client/ resolving the wrong major is rejected" 1

make_fixture "pnpm@9.15.9" "9.15.9" "9.15.9"
expect "matching major is accepted" 0

# A bare "pnpm@10" is as legal a pin as "pnpm@9.15.9"; a resolver that demands a
# minor reads no major at all and silently stops checking anything.
make_fixture "pnpm@10" "10.4.1" "10.4.1"
expect "shorthand pin, matching major, is accepted" 0

make_fixture "pnpm@10" "10.4.1" "9.15.9"
expect "shorthand pin, mismatched major, is rejected" 1

# Absent pin: nothing to enforce, so do not block setup.
make_fixture "" "11.24.0" "11.24.0"
expect "missing packageManager field is not enforced" 0

# pnpm present but --version failing (a corepack shim with no network). Must not
# trip `set -e` or invent a mismatch; the real `pnpm install` reports it better.
make_fixture "pnpm@9.15.9" "9.15.9" "9.15.9"
cat > "$FIXTURE/bin/pnpm" <<'STUB'
#!/bin/sh
exit 3
STUB
chmod +x "$FIXTURE/bin/pnpm"
expect "pnpm whose --version fails is skipped, not rejected" 0

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
