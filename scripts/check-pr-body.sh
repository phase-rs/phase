#!/usr/bin/env bash
# PR-body gate: a pull request body must be the repository's template, filled, and
# head-bound — not the output of `gh pr create --fill`.
#
# Why this exists (issue #10461): `/ship-commits` used `gh pr create --fill`, which
# builds the body from commit messages. A commit message cannot carry the two lines
# AI-CONTRIBUTOR §5-6 measures (`Gate A PASS head=<sha>`, `Final review-impl PASS
# head=<sha>`), because the SHA is the commit that carries the message. It also cannot
# carry the template's sections or the engine-implementer handoff block. The result was
# a PR body with none of the fields the review loop, artifact audit and triage read.
#
# ONE AUTHORITY, ONE ADDITION. The PARSING and the line/box rules are the repository's
# own PR-body audit (`scripts/pr_review.py`) and are imported from it — `markdown_sections`,
# `section_text`, `GATE_A_PASS_PATTERN`, `FINAL_REVIEW_IMPL_PASS_PATTERN` and
# `H2_HEADING_PATTERN`, `REQUIRED_VERIFICATION_CHECKBOX_LABELS` — so a change to the
# audit's contract cannot leave a second, drifting copy here.
#
# The addition is coverage: this gate requires EVERY `## ` section of the template, not
# only the audit's five artifact headings, because "the body is the template, filled" is
# what the issue asks. It therefore refuses two shapes the audit tolerates: a missing
# non-artifact section, and a duplicated one (a heading quoted inside a fence counts as a
# heading to the audit's own parser too, so quote the template without its `## ` prefix).
# It does NOT check the artifact content the audit checks — anchor count, `Method:` line.
# Those are the audit's, and it runs on the opened PR. (The audit also surfaces the
# `## Claimed parse impact` card list; it does not enforce it.)
#
# Usage:
#   scripts/check-pr-body.sh <body-file> [<head-sha>]
#
#   <body-file>  the file passed to `gh pr create --body-file`.
#   <head-sha>   the PR's submitted head. When given, both head-bound lines must name
#                exactly this commit; when omitted, they must still be present and
#                parseable (the pre-PR authoring case).
#
# Exit status:
#   0  PASS              - every template section present and unique, the audit's
#                          verification boxes checked, and both head-bound lines present
#                          and unique (naming <head-sha> when given). Artifact content
#                          the audit also checks (anchors, `Method:`) is out of scope
#                          here; it is checked on the opened PR.
#   1  VIOLATION         - at least one missing/duplicate section, unchecked or
#                          duplicated box, missing/duplicated head-bound line, or a
#                          head-bound line naming another commit.
#   2  INSTRUMENT FAILURE - the template, the body, the audit module or `python3` could
#                          not be read or run.
#
# Self-test: scripts/lib/pr_body_tests.sh, run by `--self-test`. No CI job invokes
# either one — wiring this gate into CI is a separate decision, and until then the
# suite runs when a contributor or a skill runs it.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TEMPLATE="$REPO_ROOT/.github/PULL_REQUEST_TEMPLATE.md"
AUDIT="$REPO_ROOT/scripts/pr_review.py"
SUITE="$SCRIPT_DIR/lib/pr_body_tests.sh"

if [ "${1:-}" = "--self-test" ]; then
    # The suite's last row re-runs this entry point; the flag stops it recursing.
    PR_BODY_SELFTEST=1 exec /bin/bash "$SUITE"
fi

if [ $# -lt 1 ] || [ $# -gt 2 ]; then
    printf 'usage: %s <body-file> [<head-sha>]\n' "$0" >&2
    exit 2
fi
BODY="$1"
HEAD_SHA="${2:-}"
if [ $# -eq 2 ] && [ -z "$HEAD_SHA" ]; then
    printf '✗ the head argument is empty; pass the 40-hex head or omit it\n' >&2
    exit 2
fi

[ -f "$TEMPLATE" ] || { printf '✗ template not found: %s\n' "$TEMPLATE" >&2; exit 2; }
[ -f "$BODY" ] || { printf '✗ body file not found: %s\n' "$BODY" >&2; exit 2; }
[ -f "$AUDIT" ] || { printf '✗ PR-body audit not found: %s\n' "$AUDIT" >&2; exit 2; }

python3 - "$BODY" "$TEMPLATE" "$AUDIT" "$HEAD_SHA" <<'PY_CHECK'
import os
import re
import sys

body_path, template_path, audit_path, head_sha = sys.argv[1:5]

# The audit imports siblings (`pr_review_dashboard`), so its own directory goes on the
# path and it is imported normally — one authority, not a copy of its parsing.
sys.path.insert(0, os.path.dirname(os.path.abspath(audit_path)))
try:
    import pr_review as audit
except Exception as error:  # noqa: BLE001 - any import failure is an instrument fault
    print(f"✗ could not load the PR-body audit: {error}", file=sys.stderr)
    raise SystemExit(2)

try:
    body = open(body_path, encoding="utf-8", errors="replace").read()
    template = open(template_path, encoding="utf-8").read()
except OSError as error:
    print(f"✗ could not read the body file: {error}", file=sys.stderr)
    raise SystemExit(2)

failures: list[str] = []

# The required section list is DERIVED from the template, never restated here: a
# section added there is required the same day.
h2 = audit.H2_HEADING_PATTERN
required: list[str] = []
# Messages name the template's own spelling, not the casefolded key.
display: dict[str, str] = {}
for line in template.splitlines():
    match = h2.match(line.strip())
    if match:
        key = match.group(1).casefold()
        display.setdefault(key, line.strip())
        required.append(key)
if not required:
    print(f"✗ the template holds no `## ` section: {template_path}", file=sys.stderr)
    raise SystemExit(2)

sections = audit.markdown_sections(body)
counts: dict[str, int] = {}
for line in body.splitlines():
    match = h2.match(line.strip())
    if match:
        key = match.group(1).casefold()
        counts[key] = counts.get(key, 0) + 1

for heading in required:
    if counts.get(heading, 0) == 0:
        failures.append(f"missing template section: {display.get(heading, heading)}")
    elif counts[heading] > 1:
        failures.append(
            f"duplicate template section: {display.get(heading, heading)}"
            " (a PR-body reader keeps only one)"
        )

# The verification boxes, by the audit's own rule: each required label appears exactly
# once as `- [x] <label>` in `## Verification`, and never as `- [ ] <label>`.
verification = audit.section_text(sections, "verification")
for label in audit.REQUIRED_VERIFICATION_CHECKBOX_LABELS:
    checked = sum(1 for line in verification.splitlines() if line.strip() == f"- [x] {label}")
    unchecked = sum(1 for line in verification.splitlines() if line.strip() == f"- [ ] {label}")
    if unchecked:
        failures.append(f"unchecked verification box: {label}")
    if checked == 0:
        failures.append(f"missing checked verification box: {label}")
    elif checked > 1:
        failures.append(f"duplicated checked verification box: {label}")

# The head-bound lines, by the audit's own regexes, read from their own sections.
gate_pattern = audit.GATE_A_PASS_PATTERN
review_pattern = audit.FINAL_REVIEW_IMPL_PASS_PATTERN
expected = head_sha.lower()
for label, heading, pattern in (
    ("Gate A", "gate a", gate_pattern),
    ("Final review-impl", "final review-impl", review_pattern),
):
    matches = re.findall(pattern, audit.section_text(sections, heading))
    if not matches:
        failures.append(f"missing head-bound line in ## {heading}: {label} PASS head=<40-hex sha>")
        continue
    if len(matches) > 1:
        failures.append(f"duplicated head-bound line in ## {heading}: {label} PASS head=")
    named = matches[0][0] if isinstance(matches[0], tuple) else matches[0]
    if head_sha and named != expected:
        failures.append(f"{label} names head {named}, but the submitted head is {expected}")

if failures:
    for failure in failures:
        print(f"✗ {failure}", file=sys.stderr)
    print(
        "\nThe PR body must be .github/PULL_REQUEST_TEMPLATE.md, filled. `gh pr create"
        " --fill` builds the body from commit messages and cannot satisfy this gate.",
        file=sys.stderr,
    )
    # 10, not 1: an unexpected exception must not be mistaken for a body violation.
    raise SystemExit(10)

if head_sha:
    print(f"PR body PASS head={expected} ({len(required)} template sections)")
else:
    print(f"PR body PASS ({len(required)} template sections)")
raise SystemExit(0)
PY_CHECK

status=$?
case "$status" in
    0) exit 0 ;;
    10) exit 1 ;;
    2) exit 2 ;;
    *)
        printf '✗ could not run the body check (python3 exit %s)\n' "$status" >&2
        exit 2
        ;;
esac
