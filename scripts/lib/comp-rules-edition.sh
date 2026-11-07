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
  local url="$1" dest="$2" bom
  if ! command -v curl >/dev/null 2>&1; then
    echo "ERROR: 'curl' is required to download the Comprehensive Rules" >&2
    return 1
  fi
  # WotC's published URL may contain unescaped spaces ("MagicCompRules 20260227.txt").
  curl -fsSL "${url// /%20}" -o "$dest"
  # Detect UTF-16 by its byte-order mark rather than `file`, which slim
  # containers often lack. Text that is neither UTF-8 nor BOM-marked UTF-16
  # carries no readable effective date, so the caller's edition check rejects it.
  bom="$(head -c 2 "$dest" | od -An -tx1 | tr -d ' \n')"
  if [ "$bom" = fffe ] || [ "$bom" = feff ]; then
    if ! command -v iconv >/dev/null 2>&1; then
      echo "ERROR: $dest is UTF-16 and 'iconv' is required to convert it" >&2
      return 1
    fi
    # mktemp creates the file exclusively under an unpredictable name, so a
    # name planted in a shared temp directory cannot redirect the write.
    # CR_CONVERT_TMP is global so the caller's EXIT trap can remove it.
    CR_CONVERT_TMP="$(mktemp "$dest.utf8.XXXXXX")"
    chmod 644 "$CR_CONVERT_TMP"
    if ! iconv -f UTF-16 -t UTF-8 "$dest" > "$CR_CONVERT_TMP" \
      || ! mv "$CR_CONVERT_TMP" "$dest"; then
      rm -f "$CR_CONVERT_TMP"
      echo "ERROR: could not convert $dest from UTF-16" >&2
      return 1
    fi
  fi
}
