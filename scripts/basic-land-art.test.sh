#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
catalog="$SCRIPT_DIR/../crates/engine/data/basic-land-art-v1.json"
validator="$SCRIPT_DIR/validate-basic-land-art.sh"
generator="$SCRIPT_DIR/gen-scryfall-printings.sh"

dir=$(mktemp -d)
trap 'rm -rf -- "$dir"' EXIT
cp "$catalog" "$dir/original-catalog.json"

oracle=46a07b53-ff58-4bd6-80dd-ded2eb0e29a3
barry=00000000-0000-0000-0000-000000000001
alias_id=d8c0de06-7782-4d2b-93ba-ef42cd5f79b4
alias_art=134a9ef4-e082-4518-8903-b2aaf47748f1

# Oracle IDs from MTGJSON AtomicCards, fixed independently of the catalog under test.
jq -n --arg barry "$barry" '
  {
    "Plains": "bc71ebf6-2056-41f7-be35-b2e5c34afa99",
    "Island": "b2c6aa39-2d2a-459c-a555-fb48ba993373",
    "Swamp": "56719f6a-1a6c-4c0a-8d21-18f7d7350b68",
    "Mountain": "a3fb7228-e76b-4e96-a40e-20b5fed75685",
    "Forest": "b34bb2dc-c1af-4d77-b0b3-a0fb342a5fc6",
    "Snow-Covered Plains": "ac8cc74d-e43b-4118-bba0-dfa8b9c04d45",
    "Snow-Covered Island": "5b2460a5-6ae5-4cad-ba94-1a9e98e6e4c0",
    "Snow-Covered Swamp": "d8239a86-7184-4005-ba1e-2dddcd756c47",
    "Snow-Covered Mountain": "ca9f660b-e07d-4f42-a46e-abd0ca72510c",
    "Snow-Covered Forest": "5f0d3be8-e63e-4ade-ae58-6b0c14f2ce6d",
    "Wastes": "05d24b0c-904a-46b6-b42a-96a4d91a0dd4",
    "Snow-Covered Wastes": "46a07b53-ff58-4bd6-80dd-ded2eb0e29a3",
    "Barry\u0027s Land": $barry
  }
  | {data: map_values([{layout: "normal", types: ["Land"], supertypes: ["Basic"],
      identifiers: {scryfallOracleId: .}}])}
' > "$dir/atomic.json"

jq --arg oracle "$oracle" --arg alias_id "$alias_id" --arg alias_art "$alias_art" '
  def printing($oracle; $id; $art; $set; $number):
    {oracle_id: $oracle, id: $id, illustration_id: $art, set: $set, collector_number: $number,
      lang: "en", games: ["paper"], layout: "normal", released_at: "2024-01-01",
      set_name: "Fixture Set", border_color: "black",
      image_uris: {normal: "https://img.example/normal.jpg", art_crop: "https://img.example/art.jpg"}};
  [.lands | to_entries[] | .key as $key
    | (.value.choices[] | printing($key; .scryfall_id; .illustration_id; .set; .collector_number)),
      (.value.aliases | to_entries[] | select(.key != "mh3/439")
        | (.key | split("/")) as [$set, $number]
        | printing($key; .key; .value; $set; $number))]
  + [printing($oracle; $alias_id; $alias_art; "mh3"; "439")]
' "$catalog" > "$dir/bulk.json"

bulk_sha=$(sha256sum < "$dir/bulk.json")
fixture_catalog="$dir/fixture-catalog.json"
jq --arg sha "${bulk_sha%% *}" '
  .source |= (.scryfall_bulk_timestamp = null | .scryfall_bulk_sha256 = $sha)
' "$catalog" > "$fixture_catalog"

expect_failure() {
  local expected=$1
  shift
  if "$@" > "$dir/log" 2>&1; then
    echo "Unexpected success from $*" >&2
    exit 1
  fi
  if ! grep -Fq -- "$expected" "$dir/log"; then
    cat "$dir/log" >&2
    echo "Expected diagnostic $expected" >&2
    exit 1
  fi
}

source_check() {
  "$validator" source "$1" "$dir/atomic.json" "$dir/bulk.json"
}

generate() {
  SCRYFALL_DEFAULT_CARDS_FILE="$1" SCRYFALL_PRINTINGS_OUTPUT="$2" "$generator"
}

mutate_catalog() {
  jq --arg oracle "$oracle" --arg barry "$barry" --arg alias_id "$alias_id" --arg alias_art "$alias_art" \
    "$1" "$fixture_catalog" > "$dir/mutated-catalog.json"
}

source_check "$fixture_catalog"
expect_failure "Scryfall bulk SHA-256 mismatch" source_check "$catalog"
mutate_catalog 'del(.source.scryfall_bulk_sha256)'
expect_failure "Expected a 64-hex Scryfall bulk SHA-256" source_check "$dir/mutated-catalog.json"
mutate_catalog '.source.scryfall_bulk_sha256 = "a"'
expect_failure "Expected a 64-hex Scryfall bulk SHA-256" source_check "$dir/mutated-catalog.json"
mutate_catalog '.source.scryfall_bulk_sha256 += "\n"'
expect_failure "Expected a 64-hex Scryfall bulk SHA-256" source_check "$dir/mutated-catalog.json"
mutate_catalog 'del(.lands[$oracle])'
expect_failure "catalog launch membership mismatch" source_check "$dir/mutated-catalog.json"
mutate_catalog '.lands[$barry] = .lands[$oracle] | del(.lands[$oracle])'
expect_failure "catalog launch membership mismatch" source_check "$dir/mutated-catalog.json"
mutate_catalog '.lands[$oracle].choices[0].scryfall_id = $alias_id'
expect_failure "$oracle mh3/229: choice printing ID mismatch" source_check "$dir/mutated-catalog.json"
mutate_catalog '.lands[$oracle] |= (.aliases["mh3/439"] = .choices[1].illustration_id)'
expect_failure "$oracle mh3/439: alias illustration mismatch" source_check "$dir/mutated-catalog.json"

reject_bulk() {
  local sha
  sha=$(sha256sum < "$dir/mutated-bulk.json")
  jq --arg sha "${sha%% *}" '.source.scryfall_bulk_sha256 = $sha' \
    "$fixture_catalog" > "$dir/mutated-catalog.json"
  expect_failure "$1" "$validator" source "$dir/mutated-catalog.json" "$dir/atomic.json" "$dir/mutated-bulk.json"
}
cat "$dir/bulk.json" "$dir/bulk.json" > "$dir/mutated-bulk.json"
reject_bulk "expected one Scryfall bulk JSON array"
for mutation in '.collector_number = 229' '.set = "MH3"' '.games = "paper"'; do
  jq 'map(if .set == "mh3" and .collector_number == "229" then '"$mutation"' else . end)' \
    "$dir/bulk.json" > "$dir/mutated-bulk.json"
  case "$mutation" in
    .collector_number*) reject_bulk "$oracle mh3/229: choice expected one source row, found 0" ;;
    .set*) reject_bulk "$oracle mh3/229: choice expected lowercase set code" ;;
    .games*) reject_bulk "$oracle mh3/229: choice expected English paper normal single-face printing" ;;
  esac
done
jq '.data.Plains |= {"0": .[0]}' "$dir/atomic.json" > "$dir/mutated-atomic.json"
expect_failure "expected AtomicCards array for Plains" \
  "$validator" source "$fixture_catalog" "$dir/mutated-atomic.json" "$dir/bulk.json"

index="$dir/index.json"
generate "$dir/bulk.json" "$index"
jq -e --slurpfile catalog "$catalog" '
  . as $index
  | all($catalog[0].lands | to_entries[]; .key as $key
      | all(.value.choices[]; .scryfall_id as $id | any($index[$key][]; .id == $id)))
' "$index" >/dev/null
cmp "$catalog" "$dir/original-catalog.json"

mutate_catalog '
  .lands[$oracle] |= (.choices[1] |=
    (.collector_number = "439" | .scryfall_id = $alias_id | .illustration_id = $alias_art)
    | del(.aliases["mh3/439"]))
'
"$validator" renderer "$dir/mutated-catalog.json" "$index"
expect_failure "$oracle: repeated illustration" source_check "$dir/mutated-catalog.json"

edit_index() {
  jq --arg oracle "$oracle" '
    def selected: .collector_number == "229";
    .[$oracle] |= ('"$1"')
  ' "$index" > "$dir/mutated-index.json"
}
reject_index() {
  expect_failure "$oracle mh3/229: $1" "$validator" renderer "$catalog" "$dir/mutated-index.json"
}
edit_index 'map(select(selected | not))'
reject_index "expected one renderer row, found 0"
edit_index '. + map(select(selected))'
reject_index "expected one renderer row, found 2"
edit_index 'map(if selected then .id = "wrong" else . end)'
reject_index "printing ID mismatch"
edit_index 'map(if selected then del(.faces[0].normal) else . end)'
reject_index "unusable faces[0].normal URL"
edit_index 'map(if selected then del(.faces[0].art_crop) else . end)'
reject_index "unusable faces[0].art_crop URL"
edit_index 'map(if selected then .faces[0].normal = "https://errors.scryfall.com/soon.jpg" else . end)'
reject_index "unusable faces[0].normal URL"
edit_index 'map(if selected then .faces[0].normal = "https://errors.scryfall.com/a/../soon.jpg" else . end)'
reject_index "unusable faces[0].normal URL"
edit_index 'map(if selected then .set = "MH3" else . end)'
reject_index "expected one renderer row, found 0"
edit_index 'map(if selected then .faces |= ([{}] + .) else . end)'
reject_index "unusable faces[0].normal URL"
edit_index 'map(if selected then .faces[0].normal = "https:///no-host.jpg" else . end)'
reject_index "unusable faces[0].normal URL"
edit_index 'map(if selected then .faces[0].normal = "https://[:::]/normal.jpg" else . end)'
reject_index "unusable faces[0].normal URL"
edit_index 'map(if selected then .faces[0].normal = "https://256.0.0.1/normal.jpg" else . end)'
reject_index "unusable faces[0].normal URL"
edit_index 'map(if selected then .faces[0].normal = "https://img.example:65535/normal.jpg" else . end)'
"$validator" renderer "$catalog" "$dir/mutated-index.json"
edit_index 'map(if selected then .faces[0].normal = "https://img.example:65536/normal.jpg" else . end)'
reject_index "unusable faces[0].normal URL"

jq --arg oracle "$oracle" '
  map(if .oracle_id == $oracle and .collector_number == "229" then del(.image_uris.normal) else . end)
' "$dir/bulk.json" > "$dir/bad-bulk.json"
expect_failure "$oracle mh3/229: unusable faces[0].normal URL" \
  generate "$dir/bad-bulk.json" "$dir/failed-index.json"
[[ ! -e $dir/failed-index.json ]]
[[ -z $(compgen -G "$dir/failed-index.json.*") ]]

cp "$index" "$dir/original-index.json"
echo 'invalid JSON' > "$dir/unreadable-bulk.json"
generate "$dir/unreadable-bulk.json" "$index"
cmp "$index" "$dir/original-index.json"

mkdir "$dir/bin"
export CURL_MARKER="$dir/curl-invoked"
cat > "$dir/bin/curl" <<'CURL'
#!/usr/bin/env bash
touch "$CURL_MARKER"
exit 1
CURL
chmod +x "$dir/bin/curl"
# Keep even a regressed fetch-before-reuse path's data directory temporary.
cd "$dir"
PATH="$dir/bin:$PATH" generate "$dir/absent-bulk.json" "$index"
[[ ! -e $CURL_MARKER && ! -e $dir/absent-bulk.json ]]
cmp "$index" "$dir/original-index.json"

reject_cached_index() {
  cp "$dir/mutated-index.json" "$dir/corrupt-before.json"
  expect_failure "$1" generate "$dir/unreadable-bulk.json" "$dir/mutated-index.json"
  cmp "$dir/mutated-index.json" "$dir/corrupt-before.json"
}
reject_cached_index "$oracle mh3/229: unusable faces[0].normal URL"
edit_index 'to_entries | map({key: (.key | tostring), value}) | from_entries'
reject_cached_index "renderer $oracle: expected a nonempty printing array"
cat "$index" "$index" > "$dir/mutated-index.json"
reject_cached_index "renderer index must contain exactly one JSON document"
jq '.unrelated = []' "$index" > "$dir/mutated-index.json"
reject_cached_index "renderer unrelated: expected a nonempty printing array"
for mutation in 'del(.set_name)' 'del(.border_color)' '.frame_effects = [false]' \
  '.full_art = "false"' '.faces = []' '.faces += [{orientation: null}]' '.faces[0].small = 42'; do
  jq --arg oracle "$oracle" '.unrelated = [.[$oracle][0] | '"$mutation"']' \
    "$index" > "$dir/mutated-index.json"
  reject_cached_index "renderer unrelated: invalid printing entry at row 0"
done
cmp "$catalog" "$dir/original-catalog.json"

echo "Basic-land art self-test passed."
