#!/usr/bin/env bash
# Skill-doc drift gate: asserts that .claude/skills/oracle-parser/SKILL.md
# (the declared single source of truth for the Oracle parser) still matches
# the parser source tree. Extracted from SKILL.md §12 so the check runs in CI
# (rust-lint job) instead of relying on manual discipline.
#
# Three invariant families:
#   (1) Every parser file/directory documented in SKILL.md exists.
#   (2) The load-bearing anchor symbols named in SKILL.md still live in the
#       documented files.
#   (3) The §3 priority table mirrors the `// Priority <label>:` slot comments
#       in parse_oracle_ir: labeled-row count equality, plus every code label
#       appears in the section. Cosmetic doc edits don't trip this; adding,
#       removing, or renaming a slot without updating §3 does. Unlabeled
#       interleaved handlers are documented as `| — |` rows, which the count
#       ignores.
set -euo pipefail
cd "$(dirname "$0")/.."

SKILL=".claude/skills/oracle-parser/SKILL.md"
ORACLE="crates/engine/src/parser/oracle.rs"

fail=0
err() {
  echo "✗ $1" >&2
  fail=1
}

[ -f "$SKILL" ] || { echo "✗ $SKILL not found" >&2; exit 1; }
[ -f "$ORACLE" ] || { echo "✗ $ORACLE not found" >&2; exit 1; }

# ---------------------------------------------------------------------------
# (1) Documented paths exist.
# ---------------------------------------------------------------------------
while IFS= read -r p; do
  [ -e "$p" ] || err "documented path missing: $p"
done <<'EOF'
crates/engine/src/parser/oracle.rs
crates/engine/src/parser/clause_shell.rs
crates/engine/src/parser/oracle_classifier.rs
crates/engine/src/parser/oracle_dispatch.rs
crates/engine/src/parser/oracle_special.rs
crates/engine/src/parser/oracle_trigger.rs
crates/engine/src/parser/oracle_replacement.rs
crates/engine/src/parser/oracle_condition.rs
crates/engine/src/parser/oracle_cost.rs
crates/engine/src/parser/oracle_keyword.rs
crates/engine/src/parser/oracle_casting.rs
crates/engine/src/parser/oracle_modal.rs
crates/engine/src/parser/oracle_class.rs
crates/engine/src/parser/oracle_level.rs
crates/engine/src/parser/oracle_saga.rs
crates/engine/src/parser/oracle_attraction.rs
crates/engine/src/parser/oracle_spacecraft.rs
crates/engine/src/parser/oracle_vote.rs
crates/engine/src/parser/oracle_separate_piles.rs
crates/engine/src/parser/oracle_target.rs
crates/engine/src/parser/oracle_quantity.rs
crates/engine/src/parser/oracle_util.rs
crates/engine/src/parser/swallow_check.rs
crates/engine/src/parser/oracle_ir/ast.rs
crates/engine/src/parser/oracle_ir/doc.rs
crates/engine/src/parser/oracle_ir/context.rs
crates/engine/src/parser/oracle_ir/diagnostic.rs
crates/engine/src/parser/oracle_ir/effect_chain.rs
crates/engine/src/parser/oracle_ir/trigger.rs
crates/engine/src/parser/oracle_ir/static_ir.rs
crates/engine/src/parser/oracle_ir/replacement.rs
crates/engine/src/parser/oracle_static/mod.rs
crates/engine/src/parser/oracle_static/dispatch.rs
crates/engine/src/parser/oracle_static/shared.rs
crates/engine/src/parser/oracle_static/anthem.rs
crates/engine/src/parser/oracle_static/keyword_grant.rs
crates/engine/src/parser/oracle_static/evasion.rs
crates/engine/src/parser/oracle_static/restriction.rs
crates/engine/src/parser/oracle_static/cost_mod.rs
crates/engine/src/parser/oracle_static/type_change.rs
crates/engine/src/parser/oracle_static/cda.rs
crates/engine/src/parser/oracle_static/grammar.rs
crates/engine/src/parser/oracle_static/static_helpers.rs
crates/engine/src/parser/oracle_static/loyalty.rs
crates/engine/src/parser/oracle_static/mana_transform.rs
crates/engine/src/parser/oracle_effect/mod.rs
crates/engine/src/parser/oracle_effect/conditions.rs
crates/engine/src/parser/oracle_effect/imperative.rs
crates/engine/src/parser/oracle_effect/lower.rs
crates/engine/src/parser/oracle_effect/search.rs
crates/engine/src/parser/oracle_effect/subject.rs
crates/engine/src/parser/oracle_effect/sequence.rs
crates/engine/src/parser/oracle_effect/token.rs
crates/engine/src/parser/oracle_effect/animation.rs
crates/engine/src/parser/oracle_effect/become_copy_except.rs
crates/engine/src/parser/oracle_effect/counter.rs
crates/engine/src/parser/oracle_effect/mana.rs
crates/engine/src/parser/oracle_nom/primitives.rs
crates/engine/src/parser/oracle_nom/target.rs
crates/engine/src/parser/oracle_nom/quantity.rs
crates/engine/src/parser/oracle_nom/duration.rs
crates/engine/src/parser/oracle_nom/condition.rs
crates/engine/src/parser/oracle_nom/filter.rs
crates/engine/src/parser/oracle_nom/error.rs
crates/engine/src/parser/oracle_nom/context.rs
crates/engine/src/parser/oracle_nom/bridge.rs
crates/engine/src/parser/oracle_nom/enchant.rs
crates/engine/src/parser/oracle_nom/return_as_aura.rs
crates/engine/src/parser/oracle_nom/PATTERNS.md
EOF

# ---------------------------------------------------------------------------
# (2) Documented anchor symbols exist in the documented files.
#     Format: "<grep pattern>\t<file>"
# ---------------------------------------------------------------------------
while IFS=$'\t' read -r pat file; do
  grep -q "$pat" "$file" || err "documented symbol missing: '$pat' in $file"
done <<'EOF'
fn parse_oracle_text	crates/engine/src/parser/oracle.rs
fn parse_oracle_ir	crates/engine/src/parser/oracle.rs
fn lower_oracle_ir	crates/engine/src/parser/oracle.rs
fn peel_clause	crates/engine/src/parser/clause_shell.rs
struct ClauseContext	crates/engine/src/parser/clause_shell.rs
fn is_static_pattern	crates/engine/src/parser/oracle_classifier.rs
fn is_replacement_pattern	crates/engine/src/parser/oracle_classifier.rs
fn dispatch_line_nom	crates/engine/src/parser/oracle_dispatch.rs
fn parse_effect_chain	crates/engine/src/parser/oracle_effect/mod.rs
fn parse_effect_clause	crates/engine/src/parser/oracle_effect/mod.rs
fn parse_imperative_effect	crates/engine/src/parser/oracle_effect/mod.rs
fn split_leading_conditional	crates/engine/src/parser/oracle_effect/conditions.rs
fn strip_leading_general_conditional	crates/engine/src/parser/oracle_effect/conditions.rs
fn static_condition_to_ability_condition	crates/engine/src/parser/oracle_effect/conditions.rs
fn static_condition_to_trigger_condition	crates/engine/src/parser/oracle_trigger.rs
fn static_condition_to_restriction_condition	crates/engine/src/parser/oracle_condition.rs
fn strip_trailing_duration	crates/engine/src/parser/oracle_effect/lower.rs
fn strip_leading_duration	crates/engine/src/parser/oracle_effect/lower.rs
fn parse_search_library_details	crates/engine/src/parser/oracle_effect/search.rs
fn parse_seek_details	crates/engine/src/parser/oracle_effect/search.rs
fn parse_search_destination	crates/engine/src/parser/oracle_effect/search.rs
fn strip_subject_clause	crates/engine/src/parser/oracle_effect/subject.rs
fn try_parse_subject_predicate_ast	crates/engine/src/parser/oracle_effect/subject.rs
fn try_parse_targeted_controller_gain_life	crates/engine/src/parser/oracle_effect/subject.rs
fn parse_imperative_family_ast	crates/engine/src/parser/oracle_effect/imperative.rs
fn parse_numeric_imperative_ast	crates/engine/src/parser/oracle_effect/imperative.rs
fn parse_zone_counter_ast	crates/engine/src/parser/oracle_effect/imperative.rs
fn split_clause_sequence	crates/engine/src/parser/oracle_effect/sequence.rs
fn parse_followup_continuation_ast	crates/engine/src/parser/oracle_effect/sequence.rs
fn try_parse_token	crates/engine/src/parser/oracle_effect/token.rs
fn parse_animation_spec	crates/engine/src/parser/oracle_effect/animation.rs
fn try_parse_put_counter	crates/engine/src/parser/oracle_effect/counter.rs
fn try_parse_add_mana_effect	crates/engine/src/parser/oracle_effect/mana.rs
fn parse_target	crates/engine/src/parser/oracle_target.rs
fn parse_type_phrase	crates/engine/src/parser/oracle_target.rs
fn parse_number	crates/engine/src/parser/oracle_util.rs
fn contains_possessive	crates/engine/src/parser/oracle_util.rs
fn contains_object_pronoun	crates/engine/src/parser/oracle_util.rs
fn match_phrase_variants	crates/engine/src/parser/oracle_util.rs
fn parse_trigger_line	crates/engine/src/parser/oracle_trigger.rs
fn parse_static_line	crates/engine/src/parser/oracle_static/mod.rs
fn parse_static_line_inner	crates/engine/src/parser/oracle_static/dispatch.rs
fn parse_static_line_multi	crates/engine/src/parser/oracle_static/shared.rs
fn parse_continuous_modifications	crates/engine/src/parser/oracle_static/keyword_grant.rs
fn strip_casting_prohibition_subject	crates/engine/src/parser/oracle_static/restriction.rs
fn parse_replacement_line	crates/engine/src/parser/oracle_replacement.rs
fn parse_inner_condition	crates/engine/src/parser/oracle_nom/condition.rs
pub fn parse_duration	crates/engine/src/parser/oracle_nom/duration.rs
pub fn parse_quantity_ref	crates/engine/src/parser/oracle_nom/quantity.rs
pub fn parse_number	crates/engine/src/parser/oracle_nom/primitives.rs
pub fn parse_number_or_x	crates/engine/src/parser/oracle_nom/primitives.rs
pub fn parse_color	crates/engine/src/parser/oracle_nom/primitives.rs
pub fn parse_mana_cost	crates/engine/src/parser/oracle_nom/primitives.rs
pub fn scan_at_word_boundaries	crates/engine/src/parser/oracle_nom/primitives.rs
fn oracle_err	crates/engine/src/parser/oracle_nom/error.rs
pub type OracleError	crates/engine/src/parser/oracle_nom/error.rs
pub type OracleResult	crates/engine/src/parser/oracle_nom/error.rs
pub fn nom_on_lower	crates/engine/src/parser/oracle_nom/bridge.rs
EOF

# ---------------------------------------------------------------------------
# (3) §3 priority table sync with `// Priority <label>:` slot comments.
# ---------------------------------------------------------------------------
code_slots=$(grep -cE '// Priority [^:]+:' "$ORACLE" || true)
section=$(awk '/^## 3\./{f=1} /^## 4\./{f=0} f' "$SKILL")
doc_rows=$(printf '%s\n' "$section" | grep -cE '^\| `' || true)

if [ "$code_slots" -eq 0 ]; then
  err "no '// Priority <label>:' comments found in $ORACLE — invariant regex needs updating"
fi
if [ "$code_slots" -ne "$doc_rows" ]; then
  err "priority table drift: $ORACLE has $code_slots '// Priority <label>:' slots but SKILL.md §3 has $doc_rows labeled rows — regenerate the §3 table"
fi

while IFS= read -r label; do
  [ -n "$label" ] || continue
  if ! printf '%s\n' "$section" | grep -qF "| \`$label\`"; then
    err "priority slot '$label' exists in $ORACLE but has no \`$label\` row in SKILL.md §3"
  fi
done < <(grep -oE '// Priority [^:]+:' "$ORACLE" | sed -E 's#// Priority ##; s#:$##' | sort -u)

if [ "$fail" -ne 0 ]; then
  echo "✗ STALE — update .claude/skills/oracle-parser/SKILL.md (see §12)" >&2
  exit 1
fi
echo "✓ oracle-parser skill references valid"
