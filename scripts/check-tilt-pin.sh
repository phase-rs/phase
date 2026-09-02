#!/usr/bin/env bash
# Static assertion that the Cloud Agent bootstrap installs tilt only from a
# digest-verified archive.
#
# .cursor/environment.json runs .cursor/install.sh automatically, and that
# script `sudo install`s the tilt binary to /usr/local/bin. A tampered or
# substituted release would therefore become root-level code on every new
# environment. The single defense is a pinned SHA-256 checked before install
# (mirroring .github/actions/binaryen). This gate fails closed if that
# version/digest/verify triple is ever weakened — e.g. someone bumps
# TILT_VERSION without updating TILT_SHA256, drops the `sha256sum -c` check,
# reorders it after the install, or reverts to piping `curl` straight into
# `tar`. Runtime `set -euo pipefail` catches a bad pair on execution; this
# catches it at review time.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL="$ROOT/.cursor/install.sh"

fail() {
  echo "check-tilt-pin: $1" >&2
  exit 1
}

[ -f "$INSTALL" ] || fail ".cursor/install.sh not found"

# 1. Version and a 64-hex-char digest must both be pinned.
grep -Eq '^TILT_VERSION="[0-9]+\.[0-9]+\.[0-9]+"' "$INSTALL" \
  || fail "TILT_VERSION is not pinned to an x.y.z release in .cursor/install.sh"
grep -Eq '^TILT_SHA256="[0-9a-f]{64}"' "$INSTALL" \
  || fail "TILT_SHA256 is not pinned to a 64-hex-char digest in .cursor/install.sh"

# 2. The archive must be downloaded to a file, never piped straight into an
#    extractor before it has been verified.
if grep -Eq 'curl[^|]*\|[[:space:]]*tar' "$INSTALL"; then
  fail "curl is piped directly into tar; download to a file and verify the digest first"
fi

# 3. The verification must use the pinned digest via `sha256sum -c`, and it must
#    run BEFORE the binary is installed to /usr/local/bin.
verify_line="$(grep -nE '\$\{?TILT_SHA256\}?.*\|[[:space:]]*sha256sum -c' "$INSTALL" | head -1 | cut -d: -f1)"
[ -n "$verify_line" ] \
  || fail "no 'sha256sum -c' verification of \$TILT_SHA256 found before install"

install_line="$(grep -nE 'sudo install .*/usr/local/bin/tilt' "$INSTALL" | head -1 | cut -d: -f1)"
[ -n "$install_line" ] \
  || fail "no 'sudo install ... /usr/local/bin/tilt' step found"

[ "$verify_line" -lt "$install_line" ] \
  || fail "digest verification (line $verify_line) must precede the tilt install (line $install_line)"

echo "check-tilt-pin PASS (tilt archive is digest-verified before install)"
