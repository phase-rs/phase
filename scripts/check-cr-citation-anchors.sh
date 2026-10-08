#!/usr/bin/env bash
# A CR citation is anchored by its rule number. A line coordinate into
# docs/MagicCompRules.txt is not an anchor: that file is gitignored, every
# contributor fetches their own copy, and the offsets move with every fetch — so
# a committed coordinate is stale on arrival and silently points at an unrelated
# rule, which is worse than no annotation. The rule number regenerates the line
# on demand: grep -n "^704.5a[. ]" docs/MagicCompRules.txt  (the bracket is
# load-bearing: parent rules are written "603.4. ", sub-rules "704.5a ", and a
# bare "^704.5a" prefix also matches 704.5aa)
#
# WHAT THIS CHECK READS. A number is readable as a coordinate only when
# something in the passage says which document it indexes. This check reads
# three such locators:
#
#     NAMED    the document's filename stands beside the number
#     LOCATED  a location word — doc(s), line(s), ln — introduces it
#     ELIDED   the document-name slot is empty: a bare ":" before the number
#
# A locator outside these three is not read, and this check claims no
# enumeration of the ones it misses: the self-check fixture below plants one,
# and every run reports it declined.
#
# NAMED needs no CR number beside it, because the numeral stands where the named
# document's own index goes: nothing but a short run of non-alphanumeric
# characters separates it from the name, or one location word from the list
# above. A word outside that list claims the numeral for something else and is
# refused — "MagicCompRules.txt: CR 704.5a" cites a rule and
# "MagicCompRules.txt (PR 4321)" cites a pull request; neither indexes the
# document. Requiring a CR number on the line instead would miss the
# coordinates whose rule number sits on the line above, which this tree carries.
# No source coordinate can reach this arm: reaching it means spelling the rules
# document's name.
#
# LOCATED and ELIDED are also how SOURCE-file
# coordinates are written — `zone_pipeline` (`:481`). The discriminator is
# ATTACHMENT, not punctuation: those two locators count when a `CR <number>`
# token stands immediately before the coordinate, separated by nothing but a
# short run of non-alphanumeric characters. Comma, backtick, "@", parenthesis
# and a comment line break are therefore all equivalent — the check has no
# opinion about which one attaches. Prose between the rule number and the
# coordinate breaks the attachment, which is what keeps
#     // CR 109.5: `entry_controller_matches` (`fn` at `:406`) answers …
# unflagged. So does a CR token that FOLLOWS the coordinate, as in
#     /// | `transformed` (`:261`, CR 712.8a) | …
#
# WHAT THAT RULE COSTS: a SOURCE coordinate written immediately after a CR
# citation IS matched, because nothing in the line says which file the numeral
# indexes. Spell it with its file — (`zone_pipeline.rs:481`) — which fills the
# name slot and takes it out of the matched set. That is the escape, and the
# failure message names it.
#
# The rule-number shape reads every sub-rule the document actually carries: its
# sub-rule letter runs are one character wide except for a single two-character
# one, which
#     grep -oP '^[0-9]{3}\.[0-9]+[a-z]+ ' docs/MagicCompRules.txt
# re-derives whenever a reader wants to check that the bound still holds.
#
# Usage: scripts/check-cr-citation-anchors.sh [path ...]      default: crates client

set -euo pipefail

# A hook runs this with git's environment exported, and GIT_INDEX_FILE is an
# absolute path in a linked worktree and under `git commit -a` / `-p`. The
# self-check below builds a throwaway repo and stages into it; with that
# variable inherited, its `git add` writes to the OUTER commit's index instead,
# leaving the caller's staged work destroyed rather than merely unstaged. The
# walk wants this repository read plainly, so drop git's ambient environment
# before anything runs a git command.
unset GIT_INDEX_FILE GIT_DIR GIT_WORK_TREE GIT_PREFIX

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# The detector. Reads file paths on stdin, writes one line per hit plus a
# trailing "#" summary. Self-check and tree walk run this identical function.
detect() {
  awk '
    # A locator+number, anywhere in TEXT, that is ATTACHED to a CR rule number
    # standing in PREFIX TEXT-so-far. Returns the arm name, or "".
    function cited(text, prefix,    rest, off, pre, ch) {
      rest = text; off = 0
      while (match(rest, /(`?:[0-9]{3,5})|((docs?|lines?|ln)[^0-9A-Za-z]{0,4}){1,2}[0-9]{3,5}/)) {
        pre = prefix substr(text, 1, off + RSTART - 1)
        ch = (length(pre) ? substr(pre, length(pre), 1) : " ")
        # left boundary: rejects `ability.rs:10450` (name slot filled) and
        # rejects "line" inside "pipeline".
        if (ch !~ /[0-9A-Za-z_.]/ &&
            pre ~ /CR [0-9]{3}(\.[0-9]+[a-z]{0,2})?[^0-9A-Za-z]{0,4}$/) return 1
        off += RSTART + RLENGTH - 1
        rest = substr(rest, RSTART + RLENGTH)
      }
      return 0
    }
    function strip(s) { sub(/^[ \t]*(\/\/!|\/\/\/|\/\/)[ \t]?/, "", s); return s }
    function scan(line, prev) {
      if (line ~ /MagicCompRules(\.txt)?[^0-9A-Za-z]{0,4}((docs?|lines?|ln)[^0-9A-Za-z]{0,4})?[0-9]{3,5}/) return "named"
      if (cited(line, "")) return "cited"
      # The attachment may cross one comment line break: same window, measured
      # over the pair with the markers removed.
      if (cited(strip(line), strip(prev) " ")) return "cited-wrapped"
      return ""
    }
    BEGIN {
      while ((getline path) > 0) {
        if (path !~ /\.(rs|ts|tsx)$/) continue
        files++; prev = ""; n = 0
        while ((getline line < path) > 0) {
          n++
          kind = scan(line, prev)
          if (kind != "") { printf "%s:%d: [%s] %s\n", path, n, kind, line; hits++ }
          if (line ~ /CR [0-9][0-9][0-9]/) citing_lines++
          prev = line
        }
        close(path)
      }
      printf "#files %d #citing_lines %d #hits %d\n", files, citing_lines, hits
    }
  '
}

# Counters come from the single "#" summary line the detector prints last.
count_of() { printf '%s\n' "$1" | sed -n 's/.*#'"$2"' \([0-9][0-9]*\).*/\1/p'; }

# verdict(): the whole path from a list of file paths on stdin to a status.
#   0  nothing matched   1  coordinates matched   2  the walk cannot answer
# Prints its projection first, then the matching citations. Every step between
# the detector's output and the status lives in here, so the self-check is a
# control over the verdict and not over the classifier alone.
verdict() {
  local out files cits hits
  out="$(detect)"
  files="$(count_of "$out" files)"
  cits="$(count_of "$out" citing_lines)"
  hits="$(count_of "$out" hits)"
  printf 'walked %s source files, read %s lines that cite a rule, matched %s\n' \
    "${files:-<no projection>}" "${cits:-<no projection>}" "${hits:-<no projection>}"
  # An absent projection is not a zero. Defaulting it would turn "the summary
  # line was never read" into "the tree is clean", which is the one verdict this
  # check must never reach by accident.
  [ -n "$files" ] && [ -n "$cits" ] && [ -n "$hits" ] || return 2
  # A walk that read no citation cannot tell a clean tree from a walk that
  # reached nothing. citing_lines only counts inside the per-file loop, so a
  # nonzero one already implies a nonzero file count and one comparison decides
  # — the self-check's citation-free leg is what drives it.
  [ "$cits" -gt 0 ] || return 2
  [ "$hits" -eq 0 ] || { printf '%s\n' "$out" | sed -n '/:[0-9][0-9]*: \[/p'; return 1; }
  return 0
}

# The script's only capture of a status. Both self-check legs and the tree walk
# reach verdict() through here, with stdin redirected rather than piped so the
# captured value survives into the caller — which is what puts the capture
# itself under the self-check's status pin.
run_verdict() { RUN_STATUS=0; RUN_REPORT="$(verdict)" || RUN_STATUS=$?; }

# --- self-check: the verdict path, in this same invocation, over planted text ---
# The fixture plants one defect per locator and per attachment shape, beside
# non-defects drawn from the HOSTILE shapes the tree actually contains — a
# source coordinate whose line also carries a CR citation before it, one that
# carries it after, a source line located by word, an issue number, a rule pair
# — plus one rules coordinate written outside what the check reads, carrying
# the tag the PASS text counts, which is the check's own demonstration that
# what it matches is bounded.
control_dir="$(mktemp -d)"
trap 'rm -rf "$control_dir"' EXIT
cat > "$control_dir/control.rs" <<'FIXTURE'
// CR 704.5a (docs/MagicCompRules.txt:5492): flagged — document NAMED, colon form.
/// CR 614.1c's enters-with template (`docs/MagicCompRules.txt` 3064): flagged — named, space form.
// CR 603.3b (docs line 2586): flagged — LOCATED by word, filename absent.
// CR 704.5a (:5492): flagged — ELIDED, attached by a parenthesis.
// CR 514.2, :2442 — flagged: same locator, attached by a comma instead.
/// (CR 115.10 @ `:886`): flagged — attached by "@" and a backtick.
/// CR 611.2a
/// (:2908, :2797): flagged — the attachment crosses a comment line break.
/// … in the order written (CR 608.2c @
/// :2793) — flagged: the break falls inside the parenthetical.
// CR 704.5aa (:5554): flagged — the document's one two-character sub-rule letter.
// The `zone_pipeline` mover (`:481`) must NOT fire — source coordinate, no CR token.
/// | `transformed` (`:261`, CR 712.8a) | must NOT fire — the CR token FOLLOWS it.
// CR 109.5: `entry_controller_matches` (`fn` at `:406`) must NOT fire — prose between.
// `casting.rs`'s builder (~line 13627, "CR 303.4a: ...") must NOT fire — source line.
// CR 603.3b (#531) and CR 601/602 must NOT fire — an issue number and a rule pair.
// DECLINED — a rules coordinate this check does not read: CR 611.2a gives the
// duration of a resolution-generated continuous effect, and its coordinate
// (:2908) stands two comment lines below the rule number, one past the join.
// CR 704.5a: a state-based action, correctly cited, must NOT fire.
// docs/MagicCompRules.txt: CR 704.5a must NOT fire — the word between the
// document name and the numeral claims the numeral as the rule number.
// CR 611.2a, docs/MagicCompRules.txt line 2908: flagged — NAMED, the numeral
// reached through a location word instead of through punctuation.
FIXTURE
declined="$(awk '/^\/\/ DECLINED/ { n++ } END { print n + 0 }' "$control_dir/control.rs")"

run_verdict < <(printf '%s\n' "$control_dir/control.rs")
control_status=$RUN_STATUS
control_report="${RUN_REPORT//"$control_dir"\//}"
control_projection="${control_report%%$'\n'*}"
# Pins WHICH lines fired and with which arm, not just how many: eight hits from
# the wrong eight lines is a broken detector that a bare count would call fine.
# This string is read back out of the report the verdict printed, so an echo
# that stops echoing empties it.
control_verdict="$(printf '%s\n' "$control_report" \
  | sed -n 's|^control\.rs:\([0-9][0-9]*\): \[\([a-z-]*\)\].*|\1=\2|p' | tr '\n' ' ')"
# Second leg: a source file that cites no rule. The fixture above has a nonzero
# file count AND a nonzero citation count, so it is blind to a refusal that
# stopped refusing; this leg's citation count is zero and it must come back
# void. Without it, "a zero from a walk that reached nothing" reads as clean.
printf 'fn silent() {}\n' > "$control_dir/silent.rs"
run_verdict < <(printf '%s\n' "$control_dir/silent.rs")
silent_status=$RUN_STATUS
silent_projection="${RUN_REPORT%%$'\n'*}"

expect_status=1
expect_projection='walked 1 source files, read 17 lines that cite a rule, matched 10'
expect_verdict='1=named 2=named 3=cited 4=cited 5=cited 6=cited 8=cited-wrapped 10=cited-wrapped 11=cited 23=named '
expect_silent_status=2
expect_silent_projection='walked 1 source files, read 0 lines that cite a rule, matched 0'
echo "cr-citation-anchors: self-check — verdict path over planted text: exit $control_status, $control_projection"
echo "cr-citation-anchors: self-check — planted per-line verdict: $control_verdict"
echo "cr-citation-anchors: self-check — citation-free walk: exit $silent_status, $silent_projection"
if [ "$control_status" != "$expect_status" ] ||
   [ "$control_projection" != "$expect_projection" ] ||
   [ "$control_verdict" != "$expect_verdict" ] ||
   [ "$silent_status" != "$expect_silent_status" ] ||
   [ "$silent_projection" != "$expect_silent_projection" ]; then
  echo "cr-citation-anchors: SELF-CHECK FAILED — the verdict path did not reproduce its planted result." >&2
  echo "    Expected exit $expect_status, '$expect_projection', '$expect_verdict'," >&2
  echo "    and exit $expect_silent_status, '$expect_silent_projection'." >&2
  echo "    The tree walk below is void: this run cannot tell a clean tree from a dead step." >&2
  exit 2
fi

# --- third leg: the exit status, read from outside the process ---
# The two statements that set this script's exit status — the final re-raise and
# any assignment to RUN_STATUS after the walk — cannot be covered from inside,
# because no process observes its own exit status. So this leg runs the script
# again as a child over a planted tree and reads the status the child exited
# with. A mutation to either statement stands in the child's copy too, so the
# child exhibits it and this run sees it. A --child argument stops the recursion; it
# is an argument rather than an environment variable so no caller's environment can
# switch this leg off.
if [ "${1:-}" != --child ]; then
  child_root="$control_dir/child"
  mkdir -p "$child_root/scripts" "$child_root/crates"
  cp "${BASH_SOURCE[0]}" "$child_root/scripts/anchors.sh"
  git -C "$child_root" init -q >/dev/null 2>&1
  child_status() {
    local st=0
    printf '%s\n' "$1" > "$child_root/crates/planted.rs"
    git -C "$child_root" add -A >/dev/null 2>&1
    "$child_root/scripts/anchors.sh" --child crates >/dev/null 2>&1 || st=$?
    printf '%s\n' "$st"
  }
  matched_status="$(child_status '// CR 704.5a (docs/MagicCompRules.txt:5492) is the state-based action.')"
  clean_status="$(child_status '// CR 704.5a: a state-based action, correctly cited.')"
  echo "cr-citation-anchors: self-check — exit status read from outside: matched tree exit $matched_status, clean tree exit $clean_status"
  if [ "$matched_status" != 1 ] || [ "$clean_status" != 0 ]; then
    echo "cr-citation-anchors: SELF-CHECK FAILED — the exit status does not follow the verdict." >&2
    echo "    A child run of this same script exited $matched_status over a planted matched tree" >&2
    echo "    (want 1) and $clean_status over a clean one (want 0)." >&2
    echo "    The tree walk below is void: its PASS would not be readable as a status." >&2
    exit 2
  fi
  # The OK line reports the two statuses this leg observed, so a run in which the
  # leg did not happen cannot print a line claiming it did.
  echo "cr-citation-anchors: self-check OK — same function and same status capture as the walk below; planted exit, projection, per-line verdict and citation-free refusal all reproduced, and a child run of this script exited $matched_status over a matched tree and $clean_status over a clean one"
fi

# --- the tree walk ---
cd "$REPO_ROOT"
[ "${1:-}" != --child ] || shift
roots=("$@"); [ "${#roots[@]}" -gt 0 ] || roots=(crates client)
run_verdict < <(git ls-files -- "${roots[@]}")
echo "cr-citation-anchors: ${RUN_REPORT%%$'\n'*} under ${roots[*]}"

if [ "$RUN_STATUS" = 2 ]; then
  echo "cr-citation-anchors: FAIL — this walk cannot answer, so its zero means nothing." >&2
  echo "    The paths argument names no source file, names files that cite no rule," >&2
  echo "    or a projection above produced no number at all." >&2
elif [ "$RUN_STATUS" = 1 ]; then
  printf '%s\n' "$RUN_REPORT" | sed -n '/:[0-9][0-9]*: \[/p' >&2
  cat >&2 <<'MSG'
cr-citation-anchors: FAIL — the CR citations above carry a line coordinate into
    docs/MagicCompRules.txt. That file is gitignored and its offsets drift with
    every fetch, so the coordinate is stale on arrival. Delete the coordinate;
    the rule number is the anchor, and
        grep -n "^<rule>[. ]" docs/MagicCompRules.txt
    regenerates the line whenever a reader actually wants it.
    If a line above points into a SOURCE file instead, it is matched because
    nothing in the line says which file the numeral indexes. Spell it with its
    file, e.g. (`zone_pipeline.rs:481`), and it leaves the matched set.
MSG
else
  echo "cr-citation-anchors: PASS — nothing this check matches stands on those lines."
  echo "    What it matches is bounded, and the self-check above measured that in this"
  echo "    same run: it declined $declined planted rules coordinate(s) written outside"
  echo "    what it reads. So PASS says what this check matched, not that the tree"
  echo "    carries no rules coordinate."
fi
# No process observes its own exit status, so this re-raise is the one statement
# no control inside this run can cover.
exit "$RUN_STATUS"
