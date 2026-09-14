#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

if ((BASH_VERSINFO[0] < 4)) || ! type -t mapfile >/dev/null ||
  ! readlink -f -- / >/dev/null 2>&1 || ! command -v sha256sum >/dev/null; then
  printf '%s\n' \
    'audit-resolution-face-casting.sh requires GNU Bash >= 4 (associative arrays and mapfile),' \
    'GNU readlink with -f, and sha256sum.' >&2
  exit 2
fi

usage() {
  printf '%s\n' \
    'usage:' \
    '  audit-resolution-face-casting.sh capture --card-data PATH --candidate-sha SHA --output PATH' \
    '  audit-resolution-face-casting.sh compare --base PATH --candidate PATH --output PATH' \
    '  audit-resolution-face-casting.sh self-test' >&2
  exit 2
}

declare -A cfg_test_module_paths=()
declare -A cfg_test_route_lines=()
declare -a cfg_test_module_records=()

classify_route_line() {
  local record=$1 path rest symbol role line_number
  path=${record%%:*}
  rest=${record#*:}
  line_number=${rest%%:*}
  case "$rest" in
    *initiate_cast_during_resolution*) symbol=initiate-cast-during-resolution ;;
    *eligible_candidates*) symbol=eligible-candidates ;;
    *"ResolutionCastRequest {"*) symbol=resolution-cast-request ;;
    *"Effect::FreeCastFromZones {"*) symbol=free-cast-from-zones ;;
    *CastFromZoneDriver*) symbol=cast-from-zone-driver ;;
    *) return 1 ;;
  esac
  if [[ $path == */tests/* ]] ||
    path_is_cfg_test_module "$path" ||
    line_is_cfg_test "$path" "$line_number"; then
    role=test
  elif [[ $rest == *'fn initiate_cast_during_resolution('* ]] ||
       [[ $rest == *'fn eligible_candidates('* ]] ||
       [[ $rest == *'struct ResolutionCastRequest'* ]] ||
       [[ $rest == *'enum CastFromZoneDriver'* ]]; then
    role=definition
  else
    role=production
  fi
  printf 'route\t%s:%s\t%s\n' "$symbol" "$role" "$record"
}

line_is_cfg_test() {
  local path=$1 line_number=$2 canonical
  canonical=$(readlink -f -- "$path") || return 1
  [[ ${cfg_test_route_lines[$canonical:$line_number]+present} ]] && return 0
  awk -v target="$line_number" '
    function sanitize(line,    out, i, c, next_c, hashes, terminator) {
      out = ""
      for (i = 1; i <= length(line); i++) {
        c = substr(line, i, 1)
        next_c = substr(line, i + 1, 1)
        if (block_comment_depth > 0) {
          if (c == "/" && next_c == "*") {
            block_comment_depth++
            i++
          } else if (c == "*" && next_c == "/") {
            block_comment_depth--
            i++
          }
          out = out " "
          continue
        }
        if (raw_terminator != "") {
          if (substr(line, i, length(raw_terminator)) == raw_terminator) {
            i += length(raw_terminator) - 1
            raw_terminator = ""
          }
          out = out " "
          continue
        }
        if (in_string) {
          if (string_escape) {
            string_escape = 0
          } else if (c == "\\") {
            string_escape = 1
          } else if (c == "\"") {
            in_string = 0
          }
          out = out " "
          continue
        }
        if (c == "/" && next_c == "/")
          break
        if (c == "/" && next_c == "*") {
          block_comment_depth = 1
          out = out " "
          i++
          continue
        }
        if ((c == "r" || (c == "b" && next_c == "r")) &&
            match(substr(line, i + (c == "b" ? 2 : 1)), /^#*"/)) {
          hashes = RLENGTH - 1
          terminator = "\""
          while (hashes-- > 0)
            terminator = terminator "#"
          raw_terminator = terminator
          i += (c == "b" ? 1 : 0) + RLENGTH
          out = out " "
          continue
        }
        if (c == "\"") {
          in_string = 1
          out = out " "
          continue
        }
        if (c == "\047" &&
            (substr(line, i + 2, 1) == "\047" ||
             (next_c == "\\" && substr(line, i + 3, 1) == "\047"))) {
          i += (next_c == "\\" ? 3 : 2)
          out = out " "
          continue
        }
        out = out c
      }
      return out
    }
    {
      code = sanitize($0)
      if (code ~ /#\[[[:space:]]*cfg[[:space:]]*\([[:space:]]*test[[:space:]]*\)[[:space:]]*\]/)
        pending_test = 1

      if (NR == target)
        exit (active_test_scopes > 0 || pending_test) ? 0 : 1

      for (i = 1; i <= length(code); i++) {
        c = substr(code, i, 1)
        if (c == "{") {
          depth++
          scope_is_test[depth] = (active_test_scopes > 0 || pending_test)
          if (scope_is_test[depth])
            active_test_scopes++
          pending_test = 0
        } else if (c == "}") {
          if (scope_is_test[depth])
            active_test_scopes--
          delete scope_is_test[depth]
          if (depth > 0)
            depth--
        }
      }
      if (pending_test && code ~ /;/)
        pending_test = 0
    }
    END {
      if (NR < target)
        exit 1
    }
  ' "$path"
}

mark_cfg_test_module_path() {
  local parent=$1 module=$2 path=$3 directory base candidate canonical changed=1
  directory=${parent%/*}
  base=${parent##*/}
  [[ $path == - ]] && path=

  if [[ -n $path ]]; then
    candidate=$directory/$path
    if [[ -f $candidate ]]; then
      canonical=$(readlink -f -- "$candidate")
      if [[ ! ${cfg_test_module_paths[$canonical]+present} ]]; then
        cfg_test_module_paths["$canonical"]=1
        changed=0
      fi
    fi
    return "$changed"
  fi

  case "$base" in
    lib.rs | main.rs | mod.rs)
      for candidate in "$directory/$module.rs" "$directory/$module/mod.rs"; do
        if [[ -f $candidate ]]; then
          canonical=$(readlink -f -- "$candidate")
          if [[ ! ${cfg_test_module_paths[$canonical]+present} ]]; then
            cfg_test_module_paths["$canonical"]=1
            changed=0
          fi
        fi
      done
      ;;
    *)
      for candidate in "$directory/${base%.rs}/$module.rs" \
        "$directory/${base%.rs}/$module/mod.rs"; do
        if [[ -f $candidate ]]; then
          canonical=$(readlink -f -- "$candidate")
          if [[ ! ${cfg_test_module_paths[$canonical]+present} ]]; then
            cfg_test_module_paths["$canonical"]=1
            changed=0
          fi
        fi
      done
      ;;
  esac
  return "$changed"
}

build_cfg_test_module_index() {
  local root parent kind module path test_only canonical_parent record changed
  local -a sources
  cfg_test_module_paths=()
  cfg_test_route_lines=()
  cfg_test_module_records=()
  sources=()
  for root in "$@"; do
    [[ -d $root ]] || continue
    while IFS= read -r parent; do
      sources+=("$parent")
    done < <(rg --files --glob '*.rs' "$root" | LC_ALL=C sort)
  done

  while IFS=$'\t' read -r kind parent module path test_only; do
    case "$kind" in
      file)
        cfg_test_module_paths["$(readlink -f -- "$parent")"]=1
        ;;
      line)
        cfg_test_route_lines["$(readlink -f -- "$parent"):$module"]=1
        ;;
      module)
        cfg_test_module_records+=("$parent"$'\t'"$module"$'\t'"$path"$'\t'"$test_only")
        [[ $test_only == 1 ]] && mark_cfg_test_module_path "$parent" "$module" "$path" || true
        ;;
    esac
  done < <(
    awk '
      function sanitize(line,    out, i, c, next_c, hashes, terminator) {
        out = ""
        for (i = 1; i <= length(line); i++) {
          c = substr(line, i, 1)
          next_c = substr(line, i + 1, 1)
          if (block_comment_depth > 0) {
            if (c == "/" && next_c == "*") {
              block_comment_depth++
              i++
            } else if (c == "*" && next_c == "/") {
              block_comment_depth--
              i++
            }
            out = out " "
            continue
          }
          if (raw_terminator != "") {
            if (substr(line, i, length(raw_terminator)) == raw_terminator) {
              i += length(raw_terminator) - 1
              raw_terminator = ""
            }
            out = out " "
            continue
          }
          if (in_string) {
            if (string_escape) {
              string_escape = 0
            } else if (c == "\\") {
              string_escape = 1
            } else if (c == "\"") {
              in_string = 0
            }
            out = out " "
            continue
          }
          if (c == "/" && next_c == "/")
            break
          if (c == "/" && next_c == "*") {
            block_comment_depth = 1
            out = out " "
            i++
            continue
          }
          if ((c == "r" || (c == "b" && next_c == "r")) &&
              match(substr(line, i + (c == "b" ? 2 : 1)), /^#*"/)) {
            hashes = RLENGTH - 1
            terminator = "\""
            while (hashes-- > 0)
              terminator = terminator "#"
            raw_terminator = terminator
            i += (c == "b" ? 1 : 0) + RLENGTH
            out = out " "
            continue
          }
          if (c == "\"") {
            in_string = 1
            out = out " "
            continue
          }
          if (c == "\047" &&
              (substr(line, i + 2, 1) == "\047" ||
               (next_c == "\\" && substr(line, i + 3, 1) == "\047"))) {
            i += (next_c == "\\" ? 3 : 2)
            out = out " "
            continue
          }
          out = out c
        }
        return out
      }
      FNR == 1 {
        block_comment_depth = 0
        raw_terminator = ""
        in_string = 0
        string_escape = 0
        pending_test = 0
        pending_path = ""
        depth = 0
        active_test_scopes = 0
      }
      {
        code = sanitize($0)
        if (code ~ /^[[:space:]]*#!\[[[:space:]]*cfg[[:space:]]*\([[:space:]]*test[[:space:]]*\)[[:space:]]*\]/)
          print "file\t" FILENAME
        if (code ~ /#\[[[:space:]]*cfg[[:space:]]*\([[:space:]]*test[[:space:]]*\)[[:space:]]*\]/)
          pending_test = 1
        is_route = (code ~ /(^|[^[:alnum:]_])initiate_cast_during_resolution[[:space:]]*\(/ ||
          code ~ /(^|[^[:alnum:]_])eligible_candidates[[:space:]]*\(/ ||
          (code ~ /(^|[^[:alnum:]_])ResolutionCastRequest[[:space:]]*\{/ &&
           code !~ /^[[:space:]]*impl.*ResolutionCastRequest[[:space:]]*\{/) ||
          code ~ /Effect::FreeCastFromZones[[:space:]]*\{/ ||
          code ~ /(^|[^[:alnum:]_])enum[[:space:]]+CastFromZoneDriver([[:space:]]|\{)/ ||
          code ~ /CastFromZoneDriver::[[:alpha:]_]/)
        if ((active_test_scopes > 0 || pending_test) && is_route)
          print "line\t" FILENAME "\t" FNR
        if (code ~ /#\[[[:space:]]*path[[:space:]]*=/) {
          raw = $0
          sub(/^[^"]*"/, "", raw)
          sub(/".*$/, "", raw)
          pending_path = raw
        }
        if (code ~ /(^|[;[:space:]])(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+[[:alpha:]_][[:alnum:]_]*[[:space:]]*;/) {
          declaration = code
          sub(/^.*(^|[;[:space:]])(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+/, "", declaration)
          sub(/[[:space:]]*;.*$/, "", declaration)
          print "module\t" FILENAME "\t" declaration "\t" (pending_path == "" ? "-" : pending_path) "\t" ((active_test_scopes > 0 || pending_test) ? 1 : 0)
          pending_test = 0
          pending_path = ""
        } else if (code !~ /^[[:space:]]*($|#\[)/) {
          pending_test = 0
          pending_path = ""
        }
        for (i = 1; i <= length(code); i++) {
          c = substr(code, i, 1)
          if (c == "{") {
            depth++
            scope_is_test[depth] = (active_test_scopes > 0 || pending_test)
            if (scope_is_test[depth])
              active_test_scopes++
            pending_test = 0
          } else if (c == "}") {
            if (scope_is_test[depth])
              active_test_scopes--
            delete scope_is_test[depth]
            if (depth > 0)
              depth--
          }
        }
        if (pending_test && code ~ /;/)
          pending_test = 0
      }
    ' "${sources[@]}"
  )

  # A cfg(test) parent makes every reachable out-of-line child test-only, so
  # propagate that fact to a fixed point for nested modules.
  while :; do
    changed=0
    for record in "${cfg_test_module_records[@]}"; do
      IFS=$'\t' read -r parent module path test_only <<<"$record"
      canonical_parent=$(readlink -f -- "$parent") || continue
      [[ ${cfg_test_module_paths[$canonical_parent]+present} ]] || continue
      if mark_cfg_test_module_path "$parent" "$module" "$path"; then
        changed=1
      fi
    done
    ((changed != 0)) || break
  done
}

path_is_cfg_test_module() {
  local canonical
  canonical=$(readlink -f -- "$1") || return 1
  [[ ${cfg_test_module_paths[$canonical]+present} ]]
}

rust_route_records() {
  awk '
    function sanitize(line,    out, i, c, next_c, hashes, terminator) {
      out = ""
      for (i = 1; i <= length(line); i++) {
        c = substr(line, i, 1)
        next_c = substr(line, i + 1, 1)
        if (block_comment_depth > 0) {
          if (c == "/" && next_c == "*") {
            block_comment_depth++
            i++
          } else if (c == "*" && next_c == "/") {
            block_comment_depth--
            i++
          }
          out = out " "
          continue
        }
        if (raw_terminator != "") {
          if (substr(line, i, length(raw_terminator)) == raw_terminator) {
            i += length(raw_terminator) - 1
            raw_terminator = ""
          }
          out = out " "
          continue
        }
        if (in_string) {
          if (string_escape) {
            string_escape = 0
          } else if (c == "\\") {
            string_escape = 1
          } else if (c == "\"") {
            in_string = 0
          }
          out = out " "
          continue
        }
        if (c == "/" && next_c == "/")
          break
        if (c == "/" && next_c == "*") {
          block_comment_depth = 1
          out = out " "
          i++
          continue
        }
        if ((c == "r" || (c == "b" && next_c == "r")) &&
            match(substr(line, i + (c == "b" ? 2 : 1)), /^#*"/)) {
          hashes = RLENGTH - 1
          terminator = "\""
          while (hashes-- > 0)
            terminator = terminator "#"
          raw_terminator = terminator
          i += (c == "b" ? 1 : 0) + RLENGTH
          out = out " "
          continue
        }
        if (c == "\"") {
          in_string = 1
          out = out " "
          continue
        }
        if (c == "\047" &&
            (substr(line, i + 2, 1) == "\047" ||
             (next_c == "\\" && substr(line, i + 3, 1) == "\047"))) {
          i += (next_c == "\\" ? 3 : 2)
          out = out " "
          continue
        }
        out = out c
      }
      return out
    }
    FNR == 1 {
      block_comment_depth = 0
      raw_terminator = ""
      in_string = 0
      string_escape = 0
      in_use = 0
    }
    {
      code = sanitize($0)
      if (in_use) {
        if (code ~ /;/)
          in_use = 0
        next
      }
      if (code ~ /^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]/) {
        if (code !~ /;/)
          in_use = 1
        next
      }
      if (code ~ /(^|[^[:alnum:]_])initiate_cast_during_resolution[[:space:]]*\(/ ||
          code ~ /(^|[^[:alnum:]_])eligible_candidates[[:space:]]*\(/ ||
          (code ~ /(^|[^[:alnum:]_])ResolutionCastRequest[[:space:]]*\{/ &&
           code !~ /^[[:space:]]*impl.*ResolutionCastRequest[[:space:]]*\{/) ||
          code ~ /Effect::FreeCastFromZones[[:space:]]*\{/ ||
          code ~ /(^|[^[:alnum:]_])enum[[:space:]]+CastFromZoneDriver([[:space:]]|\{)/ ||
          code ~ /CastFromZoneDriver::[[:alpha:]_]/)
        print FILENAME ":" FNR ":" code
    }
  ' "$@"
}

route_census() {
  local raw line classified stable
  local -a sources
  mapfile -t sources < <(
    rg --files --glob '*.rs' --glob '!**/resolution_face_census.rs' crates |
      LC_ALL=C sort
  )
  build_cfg_test_module_index crates
  raw=$(rust_route_records "${sources[@]}" | LC_ALL=C sort)
  classified=
  while IFS= read -r line; do
    [[ -n $line ]] || continue
    if ! classified+="$(classify_route_line "$line")"$'\n'; then
      printf 'unclassified production route: %s\n' "$line" >&2
      return 1
    fi
  done <<<"$raw"
  printf '%s' "$classified"
  stable=$(printf '%s' "$classified" |
    sed -E 's#^(route\t[^[:space:]]+\t[^:]+):[0-9]+:.*$#\1#' |
    LC_ALL=C sort)
  printf 'HASH\troutes_sha256\t%s\n' "$(printf '%s\n' "$stable" | sha256sum | cut -d' ' -f1)"
}

route_classifier_self_test() {
  local scratch fixture cfg_parent cfg_child nested_cfg_child file_cfg production_parent production_child
  local classified actual record result
  if classify_route_line 'crates/engine/src/synthetic.rs:1:unknown_resolution_route();' >/dev/null 2>&1; then
    printf 'synthetic unknown route unexpectedly classified\n' >&2
    return 1
  fi

  scratch=target/resolution-route-classifier-self-test.$$
  fixture=$scratch/interleaved.rs
  cfg_parent=$scratch/test_parent.rs
  cfg_child=$scratch/arbitrary_out_of_line_module.rs
  nested_cfg_child=$scratch/arbitrary_out_of_line_module/nested.rs
  file_cfg=$scratch/file_level_test_only.rs
  production_parent=$scratch/production.rs
  production_child=$scratch/production_tests.rs
  mkdir -p "$scratch/arbitrary_out_of_line_module"
  cat >"$fixture" <<'EOF'
#[cfg(test)]
mod tests {
    fn nested_test_route() {
        let _ = "{";
        let _ = r###"{"###;
        let _ = '{';
        /* { */
        let _ = ResolutionCastRequest {};
        initiate_cast_during_resolution();
    }
}

const NOT_CFG: &str = "#[cfg(test)] {";

#[cfg(test)]
fn direct_test_route() {
    initiate_cast_during_resolution();
}

struct ResolutionCastRequest {
    marker: bool,
}

impl ResolutionCastRequest {
    fn type_only() {}
}

fn initiate_cast_during_resolution() {}

/// initiate_cast_during_resolution() is documentation, not a route.
// eligible_candidates() is a comment, not a route.
use crate::{
    CastFromZoneDriver,
    ResolutionCastRequest,
};
const ROUTE_TEXT: &str = "Effect::FreeCastFromZones {";
const RAW_ROUTE_TEXT: &str = r###"CastFromZoneDriver::DuringResolution"###;

fn type_reference_only(driver: CastFromZoneDriver) {
    let _: &CastFromZoneDriver = &driver;
}

fn executable_route() {
    eligible_candidates();
    let _ = ResolutionCastRequest {};
    let _ = Effect::FreeCastFromZones {};
    let _ = CastFromZoneDriver::DuringResolution;
}

enum CastFromZoneDriver {
    DuringResolution,
}
EOF
  cat >"$cfg_parent" <<'EOF'
#[cfg(test)]
#[path = "arbitrary_out_of_line_module.rs"]
mod deliberately_unconventional_name;
EOF
  cat >"$cfg_child" <<'EOF'
mod nested;

fn arbitrary_out_of_line_test_route() {
    initiate_cast_during_resolution();
}
EOF
  cat >"$nested_cfg_child" <<'EOF'
fn nested_out_of_line_test_route() {
    initiate_cast_during_resolution();
}
EOF
  cat >"$file_cfg" <<'EOF'
#![cfg(test)]

fn file_level_test_route() {
    eligible_candidates();
}
EOF
  cat >"$production_parent" <<'EOF'
#[path = "production_tests.rs"]
mod tests;
EOF
  cat >"$production_child" <<'EOF'
fn production_route() {
    initiate_cast_during_resolution();
}
EOF
  build_cfg_test_module_index \
    crates/engine/src/game \
    crates/engine/src/parser \
    crates/engine/src/parser/oracle_effect \
    "$scratch"
  classified=$(
    rust_route_records "$fixture" "$cfg_child" "$nested_cfg_child" "$file_cfg" "$production_child" |
      LC_ALL=C sort |
      while IFS= read -r line; do classify_route_line "$line"; done
  )
  result=0
  [[ $(grep -c $'^route\tresolution-cast-request:test\t' <<<"$classified") == 1 ]] ||
    result=1
  [[ $(grep -c $'^route\tinitiate-cast-during-resolution:test\t' <<<"$classified") == 4 ]] ||
    result=1
  [[ $(grep -c $'^route\tresolution-cast-request:definition\t' <<<"$classified") == 1 ]] ||
    result=1
  [[ $(grep -c $'^route\tinitiate-cast-during-resolution:definition\t' <<<"$classified") == 1 ]] ||
    result=1
  [[ $(grep -c $'^route\tinitiate-cast-during-resolution:production\t' <<<"$classified") == 1 ]] ||
    result=1
  [[ $(grep -c $'^route\teligible-candidates:production\t' <<<"$classified") == 1 ]] ||
    result=1
  [[ $(grep -c $'^route\tresolution-cast-request:production\t' <<<"$classified") == 1 ]] ||
    result=1
  [[ $(grep -c $'^route\tfree-cast-from-zones:production\t' <<<"$classified") == 1 ]] ||
    result=1
  [[ $(grep -c $'^route\tcast-from-zone-driver:definition\t' <<<"$classified") == 1 ]] ||
    result=1
  [[ $(grep -c $'^route\tcast-from-zone-driver:production\t' <<<"$classified") == 1 ]] ||
    result=1
  [[ $(grep -c $'arbitrary_out_of_line_module.rs:4:' <<<"$classified") == 1 &&
     $(grep -c $'arbitrary_out_of_line_module/nested.rs:2:' <<<"$classified") == 1 &&
     $(grep -c $'file_level_test_only.rs:4:' <<<"$classified") == 1 ]] ||
    result=1
  [[ $(grep -c $'^route\t.*:test\t.*arbitrary_out_of_line_module\|^route\t.*:test\t.*file_level_test_only.rs' <<<"$classified") == 3 ]] ||
    result=1
  [[ $(grep -c $'interleaved.rs:[0-9]*:.*documentation\|interleaved.rs:[0-9]*:.*type_reference_only\|interleaved.rs:[0-9]*:.*ROUTE_TEXT\|interleaved.rs:[0-9]*:.*impl ResolutionCastRequest' <<<"$classified") == 0 ]] ||
    result=1
  record=$(rust_route_records "$production_child")
  [[ $(classify_route_line "$record") == $'route\tinitiate-cast-during-resolution:production\t'* ]] ||
    result=1

  actual=
  for fixture in \
    crates/engine/src/game/casting_tests.rs \
    crates/engine/src/parser/oracle_tests.rs \
    crates/engine/src/parser/oracle_effect/tests.rs; do
    record=$(rust_route_records "$fixture" | sed -n '1p')
    actual+="$(classify_route_line "$record")"$'\n'
  done
  [[ $(grep -c $':test\t' <<<"$actual") == 3 ]] || result=1
  rm -rf -- "$scratch"
  if ((result != 0)); then
    printf 'cfg(test) route classification failed:\n%s%s\n' "$classified" "$actual" >&2
    return 1
  fi
}

capture_to_output() {
  local output= arg_index temp
  local args=("$@")
  for ((arg_index = 0; arg_index + 1 < ${#args[@]}; arg_index++)); do
    if [[ ${args[arg_index]} == --output ]]; then
      output=${args[arg_index + 1]}
      break
    fi
  done
  [[ -n $output ]] || usage

  if [[ $output == /dev/stdout ]]; then
    cargo run --quiet -p phase-engine --features test-support \
      --bin resolution-face-census -- capture --output /dev/stdout "$@"
    route_census
    return
  fi

  temp="${output}.tmp.$$"
  rm -f -- "$temp"
  if ! {
    cargo run --quiet -p phase-engine --features test-support \
      --bin resolution-face-census -- capture --output /dev/stdout "$@"
    route_census
  } >"$temp"; then
    rm -f -- "$temp"
    return 1
  fi
  mv -- "$temp" "$output"
}

real_capture_compare_self_test() {
  local scratch card_data capture comparison
  scratch=target/resolution-face-census-self-test.$$
  card_data=$scratch/card-data.json
  capture=$scratch/capture.tsv
  comparison=$scratch/comparison.tsv
  mkdir -p "$scratch"
  jq 'with_entries(select(
        .key == "bind" or
        .value.scryfall_oracle_id == "ff7c12dd-1a1a-417a-b08a-d1346430858e"
      ))' client/public/card-data.json >"$card_data"
  if ! capture_to_output \
      --card-data "$card_data" \
      --candidate-sha self-test \
      --output "$capture" ||
    ! grep -Eq $'^HASH\troutes_sha256\t[0-9a-f]{64}$' "$capture" ||
    ! cargo run --quiet -p phase-engine --features test-support \
      --bin resolution-face-census -- compare \
      --base "$capture" --candidate "$capture" --output "$comparison" ||
    ! grep -q $'^SCHEMA\tresolution-face-census-v1-comparison$' "$comparison" ||
    ! grep -q $'\tunchanged$' "$comparison"; then
    rm -rf -- "$scratch"
    return 1
  fi
  rm -rf -- "$scratch"
}

command=${1:-}
shift || true
case "$command" in
  capture)
    capture_to_output "$@"
    ;;
  compare)
    cargo run --quiet -p phase-engine --features test-support --bin resolution-face-census -- compare "$@"
    route_census
    ;;
  self-test)
    route_classifier_self_test
    cargo run --quiet -p phase-engine --features test-support --bin resolution-face-census -- self-test
    real_capture_compare_self_test
    route_census >/dev/null
    printf 'audit-resolution-face-casting self-test: PASS\n'
    ;;
  *) usage ;;
esac
