# Shared helpers for the pinned Comprehensive Rules edition
# (docs/MagicCompRules.edition). Source this file; it defines functions only.

# Echo the value of KEY from the pin file at PIN ("key=value" lines; "#"
# comments and blank lines ignored). Fails if the key is absent or empty.
cr_pin_field() {
  local pin="$1" key="$2" value
  value="$(sed -n "s/^${key}=//p" "$pin" | head -n 1 | tr -d '\r')"
  if [ -z "$value" ]; then
    echo "ERROR: $pin has no '${key}=' line" >&2
    return 1
  fi
  printf '%s\n' "$value"
}

# Echo the effective date a rules text declares ("These rules are effective as
# of <date>."), or nothing if the file carries no such line. Tolerates the BOM
# and CRLF line endings of Wizards' published file.
cr_file_effective() {
  sed -n 's/^.*These rules are effective as of \(.*\)\.[[:space:]]*$/\1/p' "$1" \
    | head -n 1 | tr -d '\r'
}

# Download URL to DEST, converting Wizards' UTF-16 encoding to UTF-8 if needed.
cr_download() {
  local url="$1" dest="$2"
  # WotC's published URL may contain unescaped spaces ("MagicCompRules 20260227.txt").
  curl -fsSL "${url// /%20}" -o "$dest"
  if file "$dest" | grep -qi 'utf-16'; then
    iconv -f UTF-16 -t UTF-8 "$dest" > "$dest.utf8"
    mv "$dest.utf8" "$dest"
  fi
}
