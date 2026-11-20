#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 source CATALOG ATOMIC_CARDS DEFAULT_CARDS_OR_MTGJSON_SETS" >&2
  echo "       $0 renderer CATALOG PRINTINGS_INDEX" >&2
  exit 2
}

mode=${1:-}
case "$mode" in
  source) [[ $# == 4 ]] || usage ;;
  renderer) [[ $# == 3 ]] || usage ;;
  *) usage ;;
esac
catalog=$2

if [[ $mode == renderer ]]; then
  jq -e --slurpfile catalog "$catalog" '
    def ipv6_host:
      .[1:-1] | split("::") as $parts
      | [$parts[] | select(length > 0) | split(":")[]] as $groups
      | all($groups[]; test("^[0-9a-f]{1,4}$"; "i"))
        and (if ($parts | length) == 2 then ($groups | length) < 8
             else ($parts | length) == 1 and ($groups | length) == 8 end);

    def usable_host:
      if startswith("[") then ipv6_host
      else split(".") as $labels
        # Browsers parse hosts ending in a number as IPv4.
        | if ($labels[-1] | test("^([0-9]+|0x[0-9a-f]*)$"; "i"))
          then ($labels | length) == 4 and all($labels[];
            test("^(0|[1-9][0-9]{0,2})$") and tonumber <= 255)
          else true end
      end;

    def usable_url:
      type == "string"
      and ((capture("^https://(?<host>[a-z0-9-]+(\\.[a-z0-9-]+)*|\\[[0-9a-f:]+\\])(?::(?<port>[0-9]+))?([/?#][^[:space:][:cntrl:]]*)?\\z"; "i")
        | (.host | usable_host) and ((.port // "0") | tonumber <= 65535)) // false)
      and (test("^https://errors\\.scryfall\\.com(:[0-9]+)?/soon\\.jpg([?#]|$)"; "i") | not);

    . as $index
    | [$catalog[0].lands | to_entries[] | .key as $oracle
        | .value.choices[] as $choice
        | def fail($reason):
            error("renderer \($oracle) \($choice.set)/\($choice.collector_number): \($reason)");
        [$index[$oracle][]? | select(
          ((.set | strings | ascii_downcase) == $choice.set) and .collector_number == $choice.collector_number
        )] as $rows
        | if ($rows | length) != 1 then fail("expected one renderer row, found \($rows | length)")
          elif $rows[0].id != $choice.scryfall_id then fail("printing ID mismatch")
          elif ((try $rows[0].faces[0].normal catch null) | usable_url | not) then fail("unusable faces[0].normal URL")
          elif ((try $rows[0].faces[0].art_crop catch null) | usable_url | not) then fail("unusable faces[0].art_crop URL")
          else true end]
    | length == 24
  ' "$3" >/dev/null
  echo "Basic-land art renderer validation passed."
  exit 0
fi

atomic=$3
source_input=$4
jq -e --slurpfile catalog "$catalog" '
  def uuid: type == "string" and test("^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$");
  def printing_tuple: "\(.set)/\(.collector_number)";
  def canonical_tuple: test("^[a-z0-9]+/[^/[:space:]]+$");
  def launch_names:
    ["Plains", "Island", "Swamp", "Mountain", "Forest"]
    | . + map("Snow-Covered " + .) + ["Wastes", "Snow-Covered Wastes"];

  .data as $atomic
  | $catalog[0] as $catalog
  | [launch_names[] as $name
      | [$atomic[$name][]? | select(.layout == "normal" and (.types | index("Land"))
          and (.supertypes | index("Basic"))) | .identifiers.scryfallOracleId] | unique
      | if length == 1 and (.[0] | uuid) then .[0]
        else error("cannot resolve launch Basic Land \($name)") end] as $launch
  | if $catalog.version != 1 then error("expected catalog version 1")
    elif ($launch | unique | length) != 12 or ($catalog.lands | keys) != ($launch | sort)
      then error("catalog launch membership mismatch")
    else [$catalog.lands | to_entries[] | .key as $oracle | .value
      | def fail($reason): error("source \($oracle): \($reason)");
      if (.choices | type) != "array" or (.choices | length) != 2 then fail("expected two choices")
      elif (.aliases | type) != "object" then fail("expected aliases object")
      elif any(.choices[]; (.set | type) != "string" or (.collector_number | type) != "string"
          or (printing_tuple | canonical_tuple | not) or (.scryfall_id | uuid | not)
          or (.illustration_id | uuid | not)) then fail("invalid choice tuple or ID")
      elif ([.choices[].illustration_id] | unique | length) != 2 then fail("repeated illustration")
      elif any(.aliases | to_entries[]; (.key | canonical_tuple | not) or (.value | uuid | not))
        then fail("invalid alias tuple or illustration ID")
      elif ([.choices[] | printing_tuple] + (.aliases | keys) | unique | length) != (2 + (.aliases | length))
        then fail("tuple collision")
      else true end] | all
    end
' "$atomic" >/dev/null

# Project both supported source formats to the same printing identity fields.
if [[ -d $source_input ]]; then
  mapfile -t sets < <(jq -r '
    [.lands[] | .choices[].set, (.aliases | keys[] | split("/")[0])]
    | unique[] | ascii_upcase
  ' "$catalog")
  files=()
  for set in "${sets[@]}"; do
    files+=("$source_input/$set.json")
  done
  jq -nc --slurpfile catalog "$catalog" '
    [inputs | if .meta.version == $catalog[0].source.mtgjson_version then .data.cards[]
      else error("MTGJSON version mismatch in \(input_filename)") end
      | {
        oracle_id: .identifiers.scryfallOracleId,
        id: .identifiers.scryfallId,
        illustration_id: .identifiers.scryfallIllustrationId,
        set: .setCode,
        collector_number: .number,
        english: (.language == "English"),
        paper: (.availability | index("paper") != null),
        single_face: (.layout == "normal" and .side == null)
      }]
  ' "${files[@]}"
else
  expected_sha=$(jq -r '.source.scryfall_bulk_sha256 // empty' "$catalog")
  if [[ -n $expected_sha ]]; then
    actual_sha=$(sha256sum < "$source_input")
    if [[ ${actual_sha%% *} != "$expected_sha" ]]; then
      echo "Scryfall bulk SHA-256 mismatch" >&2
      exit 1
    fi
  fi
  jq -c 'map({
    oracle_id, id, illustration_id, set, collector_number,
    english: (.lang == "en"),
    paper: (.games | index("paper") != null),
    single_face: (.layout == "normal" and .card_faces == null)
  })' "$source_input"
fi | jq -e --slurpfile catalog "$catalog" '
  def printing_tuple: "\(.set | ascii_downcase)/\(.collector_number)";
  def source_row($rows; $oracle; $tuple; $kind):
    def fail($reason): error("source \($oracle) \($tuple): \($kind) \($reason)");
    [$rows[] | select(printing_tuple == $tuple)] as $matches
    | if ($matches | length) != 1 then fail("expected one source row, found \($matches | length)")
      elif $matches[0].oracle_id != $oracle then fail("Oracle ID mismatch")
      elif ($matches[0] | .english and .paper and .single_face | not)
        then fail("expected English paper normal single-face printing")
      else $matches[0] end;

  . as $rows
  | [$catalog[0].lands | to_entries[] | .key as $oracle | .value as $land
      | ($land.choices[] as $choice
          | ($choice | printing_tuple) as $tuple
          | source_row($rows; $oracle; $tuple; "choice")
          | if .id != $choice.scryfall_id then error("source \($oracle) \($tuple): choice printing ID mismatch")
            elif .illustration_id != $choice.illustration_id then error("source \($oracle) \($tuple): choice illustration ID mismatch")
            else true end),
        ($land.aliases | to_entries[] | .key as $tuple | .value as $illustration
          | source_row($rows; $oracle; $tuple; "alias")
          | if .illustration_id != $illustration
              or all($land.choices[]; .illustration_id != $illustration)
            then error("source \($oracle) \($tuple): alias illustration mismatch")
            else true end)]
  | all
' >/dev/null

echo "Basic-land art source validation passed."
