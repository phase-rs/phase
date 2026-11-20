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
  node - "$catalog" "$3" <<'NODE'
const { readFileSync } = require("node:fs");

function readJson(path, label) {
  try {
    return JSON.parse(readFileSync(path, "utf8").replace(/^\uFEFF/, ""));
  } catch (error) {
    throw new Error(`${label} must contain exactly one JSON document: ${error.message}`);
  }
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

// Match loadPrintingsData's whole-map admission checks in scryfall.ts.
function isImageFace(face) {
  return isRecord(face)
    && ["small", "normal", "art_crop"].every((key) => face[key] == null || typeof face[key] === "string")
    && (face.orientation === undefined || ["landscape", "portrait"].includes(face.orientation));
}

function isPrintingEntry(row) {
  return isRecord(row)
    && ["id", "set", "set_name", "collector_number", "released_at", "border_color"]
      .every((key) => typeof row[key] === "string")
    && Array.isArray(row.frame_effects) && row.frame_effects.every((effect) => typeof effect === "string")
    && typeof row.full_art === "boolean"
    && Array.isArray(row.faces) && row.faces.length > 0 && row.faces.every(isImageFace);
}

function usableUrl(value) {
  // Require an explicit authority before the URL parser repairs missing slashes.
  if (typeof value !== "string" || !/^https:\/\/[^/?#\\]+/i.test(value)
      || /[\s\x00-\x1f\x7f]/.test(value)) return false;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && url.hostname !== ""
      && !(url.hostname.replace(/\.$/, "") === "errors.scryfall.com" && url.pathname === "/soon.jpg");
  } catch {
    return false;
  }
}

try {
  const catalog = readJson(process.argv[2], "catalog");
  const index = readJson(process.argv[3], "renderer index");
  if (!isRecord(index) || Object.keys(index).length === 0) {
    throw new Error("renderer index must be a nonempty object");
  }

  for (const [oracle, rows] of Object.entries(index)) {
    if (!Array.isArray(rows) || rows.length === 0) {
      throw new Error(`renderer ${oracle}: expected a nonempty printing array`);
    }
    for (const [position, row] of rows.entries()) {
      if (!isPrintingEntry(row)) {
        throw new Error(`renderer ${oracle}: invalid printing entry at row ${position}`);
      }
    }
  }

  if (!isRecord(catalog.lands)) throw new Error("catalog lands must be an object");
  const lands = Object.entries(catalog.lands);
  if (lands.length !== 12 || lands.some(([, land]) => !Array.isArray(land.choices) || land.choices.length !== 2)) {
    throw new Error("catalog must contain 12 lands with two choices each");
  }
  for (const [oracle, land] of lands) {
    for (const choice of land.choices) {
      const tuple = `${choice.set}/${choice.collector_number}`;
      const fail = (reason) => { throw new Error(`renderer ${oracle} ${tuple}: ${reason}`); };
      const rows = (Object.hasOwn(index, oracle) ? index[oracle] : []).filter((row) =>
        row.set !== "plst" && row.set === choice.set && row.collector_number === choice.collector_number);
      if (rows.length !== 1) fail(`expected one renderer row, found ${rows.length}`);
      const [row] = rows;
      if (row.id !== choice.scryfall_id) fail("printing ID mismatch");
      for (const size of ["normal", "art_crop"]) {
        if (!usableUrl(row.faces[0][size])) fail(`unusable faces[0].${size} URL`);
      }
    }
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
NODE
  echo "Basic-land art renderer validation passed."
  exit 0
fi

atomic=$3
source_input=$4
jq -es --slurpfile catalog "$catalog" '
  def uuid: type == "string" and test("^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$");
  def printing_tuple: "\(.set)/\(.collector_number)";
  def canonical_tuple: test("^[a-z0-9]+/[^/[:space:]]+$");
  def launch_names:
    ["Plains", "Island", "Swamp", "Mountain", "Forest"]
    | . + map("Snow-Covered " + .) + ["Wastes", "Snow-Covered Wastes"];

  if ($catalog | length) != 1 then error("expected one catalog JSON document")
  elif length != 1 or (.[0].data | type) != "object"
    then error("expected one AtomicCards JSON document with a data object")
  else .[0].data end as $atomic
  | $catalog[0] as $catalog
  | [launch_names[] as $name
      | [$atomic[$name] | if type == "array" then .[] else error("expected AtomicCards array for \($name)") end
        | select(.layout == "normal" and (.types | type == "array" and index("Land") != null)
          and (.supertypes | type == "array" and index("Basic") != null)) | .identifiers.scryfallOracleId] | unique
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
    [inputs | if .meta.version != $catalog[0].source.mtgjson_version
      then error("MTGJSON version mismatch in \(input_filename)")
      elif (.data.cards | type) != "array" then error("expected MTGJSON cards array in \(input_filename)")
      else .data.cards[] end
      | {
        oracle_id: .identifiers.scryfallOracleId,
        id: .identifiers.scryfallId,
        illustration_id: .identifiers.scryfallIllustrationId,
        set: (.setCode | strings | ascii_downcase),
        collector_number: .number,
        english: (.language == "English"),
        paper: (.availability | type == "array" and index("paper") != null),
        single_face: (.layout == "normal" and .side == null)
      }]
  ' "${files[@]}"
else
  if ! expected_sha=$(jq -er '
    .source.scryfall_bulk_sha256 | strings | select(test("^[0-9a-fA-F]{64}\\z")) | ascii_downcase
  ' "$catalog"); then
    echo "Expected a 64-hex Scryfall bulk SHA-256" >&2
    exit 1
  fi
  actual_sha=$(sha256sum < "$source_input")
  if [[ ${actual_sha%% *} != "$expected_sha" ]]; then
    echo "Scryfall bulk SHA-256 mismatch" >&2
    exit 1
  fi
  jq -cs '
    if length != 1 or (.[0] | type) != "array"
      then error("expected one Scryfall bulk JSON array")
      else .[0] end
    | map({
      oracle_id, id, illustration_id, set, collector_number,
      english: (.lang == "en"),
      paper: (.games | type == "array" and index("paper") != null),
      single_face: (.layout == "normal" and .card_faces == null)
    })' "$source_input"
fi | jq -e --slurpfile catalog "$catalog" '
  def printing_tuple:
    select((.set | type) == "string" and (.collector_number | type) == "string")
    | "\(.set | ascii_downcase)/\(.collector_number)";
  def source_row($rows; $oracle; $tuple; $kind):
    def fail($reason): error("source \($oracle) \($tuple): \($kind) \($reason)");
    [$rows[] | select(printing_tuple == $tuple)] as $matches
    | if ($matches | length) != 1 then fail("expected one source row, found \($matches | length)")
      elif $matches[0].set != ($tuple | split("/")[0]) then fail("expected lowercase set code")
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
