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

jq --arg barry "$barry" '
  ["Plains", "Island", "Swamp", "Mountain", "Forest"] as $basics
  | ($basics + ($basics | map("Snow-Covered " + .)) + ["Wastes", "Snow-Covered Wastes"]) as $names
  | (.lands | keys_unsorted) as $ids
  | {data: (reduce range(0; $names | length) as $i ({};
      .[$names[$i]] = [$ids[$i]]) | .["Barry\u0027s Land"] = [$barry]
      | map_values(map({layout: "normal", types: ["Land"], supertypes: ["Basic"],
          identifiers: {scryfallOracleId: .}})))}
' "$catalog" > "$dir/atomic.json"

jq --arg oracle "$oracle" --arg alias_id "$alias_id" --arg alias_art "$alias_art" '
  def printing($oracle; $id; $art; $set; $number):
    {oracle_id: $oracle, id: $id, illustration_id: $art, set: $set, collector_number: $number,
      lang: "en", games: ["paper"], layout: "normal", released_at: "2024-01-01",
      image_uris: {normal: "https://img.example/normal.jpg", art_crop: "https://img.example/art.jpg"}};
  [.lands | to_entries[] | .key as $key
    | (.value.choices[] | printing($key; .scryfall_id; .illustration_id; .set; .collector_number)),
      (.value.aliases | to_entries[] | select(.key != "mh3/439")
        | (.key | split("/")) as [$set, $number]
        | printing($key; null; .value; $set; $number))]
  + [printing($oracle; $alias_id; $alias_art; "mh3"; "439")]
' "$catalog" > "$dir/bulk.json"

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
    "$1" "$catalog" > "$dir/mutated-catalog.json"
}

source_check "$catalog"
mutate_catalog 'del(.lands[$oracle])'
expect_failure "catalog launch membership mismatch" source_check "$dir/mutated-catalog.json"
mutate_catalog '.lands[$barry] = .lands[$oracle] | del(.lands[$oracle])'
expect_failure "catalog launch membership mismatch" source_check "$dir/mutated-catalog.json"
mutate_catalog '.lands[$oracle].choices[0].scryfall_id = $alias_id'
expect_failure "$oracle mh3/229: choice printing ID mismatch" source_check "$dir/mutated-catalog.json"
mutate_catalog '.lands[$oracle] |= (.aliases["mh3/439"] = .choices[1].illustration_id)'
expect_failure "$oracle mh3/439: alias illustration mismatch" source_check "$dir/mutated-catalog.json"

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
edit_index 'map(if selected then .faces |= ([{}] + .) else . end)'
reject_index "unusable faces[0].normal URL"
edit_index 'map(if selected then .faces[0].normal = "https:///no-host.jpg" else . end)'
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

cp "$dir/mutated-index.json" "$dir/corrupt-before.json"
expect_failure "$oracle mh3/229: unusable faces[0].normal URL" \
  generate "$dir/unreadable-bulk.json" "$dir/mutated-index.json"
cmp "$dir/mutated-index.json" "$dir/corrupt-before.json"
cmp "$catalog" "$dir/original-catalog.json"

echo "Basic-land art self-test passed."
