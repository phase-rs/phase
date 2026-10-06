#!/usr/bin/env bash
# Build the phase.rs Flatpak locally from an already-released Linux artifact.
#
# The Flatpak deliberately compiles nothing. org.gnome.Platform//50 supplies
# every library `phase-tauri` links against, so this script stages the exact
# binary the release job already produced and hands it to flatpak-builder. That
# keeps the packaged binary byte-identical to the one .deb/.AppImage users run,
# and keeps this script honest: if it built its own binary, a green build here
# would say nothing about the artifact people actually download.
#
#   ./build-local.sh --appimage ~/Downloads/Phase-Desktop-Linux-x86_64.AppImage
#   ./build-local.sh --deb ~/Downloads/Phase-Desktop-Linux-x86_64.deb --install
#
# Requires `flatpak`, `flatpak-builder` and `objdump`. See --help.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly HERE
readonly MANIFEST="$HERE/rs.phase.app.yml"
readonly STAGE="$HERE/build"
readonly BUILDDIR="$HERE/.flatpak-builder-out"
readonly REPO="$HERE/.flatpak-repo"

source_artifact=""
source_kind=""
do_install=0
do_run=0
do_bundle=0
allow_unguarded=0
built_unguarded=0

die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
note() { printf '\033[36m==>\033[0m %s\n' "$*"; }

# Read both from the manifest rather than restating them. A second copy here
# would keep validating the old runtime after a manifest bump, while
# flatpak-builder built against the new one.
manifest_value() {
  local key="$1" value
  value="$(sed -n "s/^${key}:[[:space:]]*['\"]\{0,1\}\([^'\"[:space:]]\{1,\}\)['\"]\{0,1\}[[:space:]]*\$/\1/p" "$MANIFEST")"
  [ -n "$value" ] || die "could not read '$key' from $MANIFEST"
  printf '%s' "$value"
}

usage() {
  cat <<'EOF'
Usage: build-local.sh (--appimage PATH | --deb PATH | --binary PATH) [options]

Source (exactly one required):
  --appimage PATH   Extract phase-tauri from a released .AppImage
  --deb PATH        Extract phase-tauri from a released .deb
  --binary PATH     Use an already-extracted phase-tauri binary

Options:
  --install         flatpak install the built app into the user installation
  --run             Launch the app after building (implies --install)
  --bundle          Also write <app-id>.flatpak, a single-file bundle
  --allow-unguarded-updater
                    Build even if the staged binary predates the sandbox
                    update guard. For packaging experiments only — the
                    resulting package will try to self-update. Do not ship it.
  -h, --help        Show this help

Prerequisites:
  flatpak, flatpak-builder, objdump (binutils), and the GNOME runtime + SDK
  at the version rs.phase.app.yml names. If a runtime is missing, this script
  exits with the exact `flatpak install` command for it.
EOF
}

set_source() {
  [ -z "$source_artifact" ] || die "pass only one of --appimage/--deb/--binary"
  source_kind="$1"
  [ -n "${2:-}" ] || die "--$1 needs a path"
  [ -f "$2" ] || die "no such file: $2"
  source_artifact="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --appimage) set_source appimage "${2:-}"; shift 2 ;;
    --deb)      set_source deb "${2:-}"; shift 2 ;;
    --binary)   set_source binary "${2:-}"; shift 2 ;;
    --install)  do_install=1; shift ;;
    --run)      do_run=1; do_install=1; shift ;;
    --bundle)   do_bundle=1; shift ;;
    --allow-unguarded-updater) allow_unguarded=1; shift ;;
    -h|--help)  usage; exit 0 ;;
    *)          usage >&2; die "unknown argument: $1" ;;
  esac
done

[ -n "$source_artifact" ] || { usage >&2; die "a source artifact is required"; }

# Derived only now: --help and argument errors must work even when the manifest
# is missing or malformed.
[ -f "$MANIFEST" ] || die "manifest not found: $MANIFEST"
APP_ID="$(manifest_value app-id)"
RUNTIME_VERSION="$(manifest_value runtime-version)"
readonly APP_ID RUNTIME_VERSION

command -v flatpak >/dev/null || die "flatpak is not installed"
command -v flatpak-builder >/dev/null \
  || die "flatpak-builder is not installed (try: flatpak install --user flathub org.flatpak.Builder, then use 'flatpak run org.flatpak.Builder')"
# Checked up front because the NEEDED preflight below reads objdump through a
# process substitution, where a missing binary would go unnoticed by `set -e`
# and turn that check into a silent pass.
command -v objdump >/dev/null || die "objdump is not installed (install binutils)"

for ref in "org.gnome.Platform//$RUNTIME_VERSION" "org.gnome.Sdk//$RUNTIME_VERSION"; do
  flatpak info "$ref" >/dev/null 2>&1 \
    || die "missing $ref — install it with: flatpak install --user flathub $ref"
done

note "staging phase-tauri from $(basename "$source_artifact")"
rm -rf "$STAGE"
mkdir -p "$STAGE"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

case "$source_kind" in
  binary)
    cp "$source_artifact" "$STAGE/phase-tauri"
    ;;
  appimage)
    # Release downloads arrive without the bit set, and the extract below runs
    # the artifact directly, so say why rather than dying on "Permission denied".
    [ -x "$source_artifact" ] \
      || die "AppImage is not executable — chmod +x '$source_artifact' first"
    # --appimage-extract always unpacks into ./squashfs-root of the cwd.
    (cd "$scratch" && "$source_artifact" --appimage-extract >/dev/null)
    [ -f "$scratch/squashfs-root/usr/bin/phase-tauri" ] \
      || die "no usr/bin/phase-tauri inside that AppImage"
    # Copy the binary OUT of the AppDir rather than pointing the build at it in
    # place. phase-tauri carries RUNPATH '$ORIGIN/../lib', so a copy that stays
    # next to the AppDir's bundled libraries keeps resolving against them — the
    # exact Ubuntu-built WebKitGTK this package exists to stop using.
    cp "$scratch/squashfs-root/usr/bin/phase-tauri" "$STAGE/phase-tauri"
    ;;
  deb)
    if command -v dpkg-deb >/dev/null; then
      dpkg-deb -x "$source_artifact" "$scratch/deb"
    else
      # Fedora and friends usually lack dpkg; ar + tar is in binutils/coreutils.
      (cd "$scratch" && ar x "$source_artifact")
      mkdir -p "$scratch/deb"
      data="$(find "$scratch" -maxdepth 1 -name 'data.tar.*' -print -quit)"
      [ -n "$data" ] || die "no data.tar.* member inside that .deb"
      tar -xf "$data" -C "$scratch/deb"
    fi
    binary="$(find "$scratch/deb" -type f -name phase-tauri -print -quit)"
    [ -n "$binary" ] || die "no phase-tauri inside that .deb"
    cp "$binary" "$STAGE/phase-tauri"
    ;;
esac

chmod 755 "$STAGE/phase-tauri"

# The guard that stops the shell replacing its own binaries lives *inside* the
# staged binary (client/src-tauri/src/update_authority.rs), not in this package.
# Staging a release that predates it produces a Flatpak with self-update fully
# live, which then tries to rewrite a read-only /app on every check and leaves a
# permanent update error in the UI. Refuse to build that by default.
#
# Heuristic on purpose: the guard's marker path is a string literal in the
# binary, so its presence is evidence the guard was compiled in.
note "checking the staged binary carries the sandbox update guard"
if ! grep -qa -- '/.flatpak-info' "$STAGE/phase-tauri"; then
  [ "$allow_unguarded" -eq 1 ] || die \
    "this artifact predates the Flatpak update guard, so the package would try to self-update inside the sandbox; stage a release built from update_authority.rs, or pass --allow-unguarded-updater to build it anyway"
  printf '\033[33mwarning:\033[0m staged binary has no sandbox update guard; this package will attempt to self-update. Do not distribute it.\n' >&2
  built_unguarded=1
fi

# Fail loudly here rather than at runtime inside the sandbox: if the binary ever
# grows a dependency the runtime does not carry, this is the check that says so.
note "checking the runtime satisfies every NEEDED library"
runtime_files="$(flatpak info --show-location "org.gnome.Platform//$RUNTIME_VERSION")/files"
missing=""
while read -r lib; do
  [ -n "$lib" ] || continue
  find "$runtime_files/lib" -name "$lib" -print -quit 2>/dev/null | grep -q . \
    || missing="$missing $lib"
done < <(objdump -p "$STAGE/phase-tauri" | awk '/NEEDED/{print $2}')
[ -z "$missing" ] || die "org.gnome.Platform//$RUNTIME_VERSION is missing:$missing"

note "building $APP_ID"
rm -rf "$BUILDDIR"
flatpak-builder --force-clean --repo="$REPO" "$BUILDDIR" "$MANIFEST"

if [ "$do_install" -eq 1 ]; then
  note "installing into the user installation"
  flatpak remote-add --user --no-gpg-verify --if-not-exists phase-local "$REPO"
  flatpak install --user --noninteractive --reinstall phase-local "$APP_ID"
fi

if [ "$do_bundle" -eq 1 ]; then
  note "writing $HERE/$APP_ID.flatpak"
  flatpak build-bundle "$REPO" "$HERE/$APP_ID.flatpak" "$APP_ID"
fi

if [ "$do_run" -eq 1 ]; then
  note "launching"
  flatpak run "$APP_ID"
fi

note "done"

# flatpak-builder floods the terminal, so the warning above has long scrolled
# away by now. Say it again where it is the last thing on screen.
[ "$built_unguarded" -eq 0 ] || printf \
  '\033[33mwarning:\033[0m built WITHOUT the sandbox update guard — for local experiments only, do not distribute.\n' >&2
