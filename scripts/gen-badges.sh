#!/usr/bin/env bash
set -euo pipefail

# Generates shields.io badge markdown from coverage-data.json and updates README.md.
# Run after gen-card-data.sh to keep badges in sync with coverage numbers.

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

COVERAGE_FILE="${1:-client/public/coverage-data.json}"
README="README.md"

if [ ! -f "$COVERAGE_FILE" ]; then
  echo "Error: $COVERAGE_FILE not found" >&2
  exit 1
fi

badge_color() {
  local pct="$1"
  local int_pct="${pct%.*}"
  if [ "$int_pct" -ge 90 ]; then echo "brightgreen"
  elif [ "$int_pct" -ge 80 ]; then echo "green"
  elif [ "$int_pct" -ge 70 ]; then echo "yellowgreen"
  elif [ "$int_pct" -ge 60 ]; then echo "yellow"
  else echo "orange"
  fi
}

# Extract data using python (available on all CI runners and dev machines)
read_json() {
  python3 -c "
import json, sys
data = json.load(open('$COVERAGE_FILE'))
fmt = data['coverage_by_format']

# Overall
print(f\"overall_pct={data['coverage_pct']:.0f}\")
print(f\"overall_supported={data['supported_cards']}\")
print(f\"overall_total={data['total_cards']}\")

# Per-format (sorted by coverage desc for display)
formats = sorted(fmt.items(), key=lambda x: -x[1]['coverage_pct'])
for name, info in formats:
    pct = info['coverage_pct']
    print(f\"fmt_{name}_pct={pct:.0f}\")

# Keywords
from collections import Counter
kw_labels = set()
for c in data.get('cards', []):
    for d in c.get('parse_details', []):
        if d['category'] == 'keyword':
            kw_labels.add(d['label'])
print(f\"keyword_count={len(kw_labels)}\")
"
}

eval "$(read_json)"

# Build badge img tags (HTML for centering)
BADGES=""

# Overall coverage badge
color=$(badge_color "$overall_pct")
BADGES+="  <img alt=\"Card Coverage\" src=\"https://img.shields.io/badge/card_coverage-${overall_pct}%25-${color}\">"
BADGES+=$'\n'

# Keywords badge
BADGES+="  <img alt=\"Keywords\" src=\"https://img.shields.io/badge/keywords-${keyword_count}%2F${keyword_count}-brightgreen\">"
BADGES+=$'\n'

# Cards badge
BADGES+="  <img alt=\"Cards\" src=\"https://img.shields.io/badge/cards-${overall_supported}%2F${overall_total}-${color}\">"

# Format badges
FORMAT_BADGES=""
for format in pauper modern pioneer legacy vintage commander standard; do
  var="fmt_${format}_pct"
  pct="${!var}"
  color=$(badge_color "$pct")
  label="$(echo "$format" | python3 -c "print(input().capitalize())")"
  FORMAT_BADGES+="  <img alt=\"${label}\" src=\"https://img.shields.io/badge/${label}-${pct}%25-${color}\">"
  FORMAT_BADGES+=$'\n'
done

# Generate the badge block
BADGE_BLOCK="<!-- coverage-badges:start -->
<p align=\"center\">
${BADGES}
  <br/>
${FORMAT_BADGES%$'\n'}
</p>
<!-- coverage-badges:end -->"

# Update README.md between markers, or insert after the description paragraph
if grep -q "coverage-badges:start" "$README"; then
  # Replace existing block
  python3 -c "
import re, sys
readme = open('$README').read()
badge_block = '''$BADGE_BLOCK'''
pattern = r'<!-- coverage-badges:start -->.*?<!-- coverage-badges:end -->'
updated = re.sub(pattern, badge_block, readme, flags=re.DOTALL)
open('$README', 'w').write(updated)
"
  echo "Updated existing badge block in $README"
else
  # Insert after the nav links (the <p> with Quick Start links)
  python3 -c "
import sys
lines = open('$README').readlines()
badge_block = '''$BADGE_BLOCK'''
insert_idx = None
for i, line in enumerate(lines):
    if '<a href=\"#quick-start\">Quick Start</a>' in line:
        # Find the closing </p> after this line
        for j in range(i, min(i+3, len(lines))):
            if '</p>' in lines[j]:
                insert_idx = j + 1
                break
        break
if insert_idx is None:
    print('Warning: could not find insertion point, appending', file=sys.stderr)
    insert_idx = 0
lines.insert(insert_idx, '\n' + badge_block + '\n')
open('$README', 'w').write(''.join(lines))
"
  echo "Inserted badge block into $README"
fi
