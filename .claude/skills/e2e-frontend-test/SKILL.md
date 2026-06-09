---
name: e2e-frontend-test
description: Manually exercise a frontend PR end-to-end against the running phase.rs app, capture before/after screenshots for every scenario, and post a PR comment with the evidence embedded inline. Use when a PR touches client/src/** and you want durable visual proof on the PR that the change works (and that the negative case is correctly rejected). Complement to `verify` and `run` — those launch and observe; this one produces persistent PR evidence.
---

# E2E Frontend Test

Take a frontend PR, drive the running phase.rs app through a written scenario
plan, capture before/after screenshots at every state-changing step, upload them
to a sidecar branch, and post a single PR comment with the evidence inlined.

The deliverable is the PR comment. Local test logs are scratch — the persistent
artifact is what a human reviewer will see on GitHub two weeks from now when
they ask "was this actually tested?"

## When to use

- A PR touches `client/src/**` and a visual artifact would help the reviewer.
- The change is observable in the UI (overlay, modal, animation, game flow).
- You want a reproducible scenario record on the PR for future audits.

## When NOT to use

- Pure engine/parser changes with no UI surface — `pr-contribution-handler`
  covers those with diff review + Tilt verification.
- Quick local sanity check with no plan to record evidence — use `/verify` or
  `/run` instead.
- Multiplayer/networking-only changes — those need `phase-server` running and
  multi-tab orchestration that this skill doesn't currently script.

## Prerequisites

- Tilt must be up (`tilt up` in repo root, or already running).
- `vite` Tilt resource must be serving on `http://localhost:5173`. Verify:
  ```bash
  curl -sf -o /dev/null -w "%{http_code}\n" http://localhost:5173
  ```
- `agent-browser` available (see `agent-browser` skill).
- `gh` authenticated against `phase-rs/phase`.

## Step 0 — Resolve the target

```bash
# If the user gave a PR number:
PR_NUMBER="${1:-$(gh pr list --head $(git branch --show-current) --json number --jq '.[0].number')}"
REPO="phase-rs/phase"

PR_TITLE=$(gh pr view "$PR_NUMBER" --repo "$REPO" --json title --jq '.title' \
  | tr '[:upper:]' '[:lower:]' \
  | sed 's/[^a-z0-9]/-/g' \
  | sed 's/--*/-/g' \
  | sed 's/^-//;s/-$//' \
  | head -c 50)

REPO_ROOT=$(git rev-parse --show-toplevel)
RESULTS_DIR="$REPO_ROOT/test-results/PR-${PR_NUMBER}-${PR_TITLE}"
mkdir -p "$RESULTS_DIR"
```

`test-results/` is gitignored; the screenshots are local-only until Step 6
uploads them to the sidecar branch.

## Step 1 — Understand the PR

```bash
gh pr view "$PR_NUMBER" --repo "$REPO" --json body --jq '.body'
git log --oneline main..HEAD | head -20
git diff main --stat
```

Read for:

- **Why** does this PR exist? What player-visible behavior changes?
- **What** is the user-facing surface — which page, which component, which
  interaction?
- **How** is it implemented — which engine action(s) does it dispatch, which
  state slices does it subscribe to?
- **Risks** — are there siblings on the same screen this change could regress?
  Game-state edge cases (empty hand, 0 life, exiled commander, etc.)?

If the diff is engine-only with no `client/src/` changes, stop here and tell the
user this skill isn't the right tool.

## Step 2 — Write the scenario plan

Write `$RESULTS_DIR/test-plan.md` before driving the browser. Forcing the plan
up front catches missing negative cases and ambiguous "expected" criteria.

```markdown
# Test Plan: PR #{N} — {title}

## Scope
- Pages touched: /game, /setup, …
- Engine actions exercised: CastSpell, …
- State-changing UI: hand zone, stack, animation queue, …

## Scenarios
1. **Golden path:** {what the user is supposed to be able to do}
   - Before: {state of UI before action}
   - Action: {exact clicks / inputs}
   - Expected: {what should change in the DOM, engine state, animation}
   - Screenshot before: 01-{scenario}-before.png
   - Screenshot after: 02-{scenario}-after.png

2. **Edge case:** {boundary condition — empty zone, full zone, last card, etc.}
   ...

## Negative scenarios (REQUIRED — at least one)
1. {action that should be rejected}
   - Action: {trigger the illegal state}
   - Expected: action ignored / error message / state unchanged
   - Screenshot evidence: 0X-negative-{description}.png
```

Be critical. Add at least one negative scenario per feature — if the user
*could* trigger the wrong state, the test must verify the engine/UI rejects it.

## Step 3 — Drive the browser

Start a named session so cookies / localStorage / IndexedDB game state persists
across `agent-browser` calls.

```bash
# Close any prior session
agent-browser close 2>/dev/null || true

agent-browser --session-name phase-e2e open 'http://localhost:5173' --timeout 15000

# Snapshot to discover refs
agent-browser --session-name phase-e2e snapshot | grep -E "button|link|text:"
```

### Pages reference

| Page | Path | Use for |
|------|------|---------|
| Menu | `/` | Entry, "New Game", "Multiplayer", "Decks" buttons |
| Game setup | `/setup` | Pick deck, opponent, difficulty |
| Game in progress | `/game/{id}` | Board, hand, stack, combat, targeting overlays |
| Multiplayer lobby | `/multiplayer` | Lobby UI, hosting/joining flows |
| Deck builder | `/deck-builder` | Card search, deck composition, format checks |
| My decks | `/my-decks` | Saved decks list, import/export |
| Coverage | `/coverage` | Engine coverage report (rarely needs UI testing) |
| Draft | `/draft`, `/draft/quick`, `/draft-pod` | Draft simulator |

### Setting up a game state

Most scenarios start from a clean game. The fastest path:

1. Navigate to `/setup`.
2. Pick a deck (use a known-good preset if the PR doesn't change deck loading).
3. Pick opponent + difficulty.
4. Click "Start Game" — wait for the board to render.
5. The board URL becomes `/game/{id}` — capture the id if you need to
   restart/restore the same session.

### Driving the engine via the UI

Phase.rs frontend is a strict display layer (see CLAUDE.md). All game state
manipulation goes through the engine adapter — you cannot inject state
client-side. To set up a specific scenario:

- Use the AI opponent's deck/difficulty knobs at `/setup` to bias the state.
- Play turns with `Pass` (spacebar by default; verify via `agent-browser snapshot`).
- For deterministic scenarios, the engine accepts a seed at game creation — check
  `gameStore`/adapter for how to pass it if your test needs reproducibility.

If a scenario requires precise game state (e.g., "verify Lightning Bolt deals 3
damage to a creature with toughness 4"), the cheap path is to drive the AI into
that state via play. The expensive path is to add a debug action to the engine
or a test-mode initial-state hook. Don't do the expensive path inline — file a
follow-up and pick a different scenario that exercises the same code path.

### Capturing engine state evidence

Beyond screenshots, capture store state only when the running dev build exposes
a debug store global such as `window.__gameStore`. Zustand stores are
module-scoped imports by default; do not assume `useGameStore` exists on
`window` unless the app explicitly exposes it for that run.

```bash
# Get current game state from the debug store (browser console eval)
agent-browser --session-name phase-e2e eval \
  "JSON.stringify(window.__gameStore?.getState?.().gameState ?? null, null, 2)" \
  > $RESULTS_DIR/0X-state-before.json

# Same after the action
agent-browser --session-name phase-e2e eval \
  "JSON.stringify(window.__gameStore?.getState?.().gameState ?? null, null, 2)" \
  > $RESULTS_DIR/0X-state-after.json
```

For each state-changing scenario, capture both the screenshot pair AND the
JSON state pair when the debug store is available. The JSON catches bugs the
screenshot can't (wrong life total, wrong P/T, wrong zone membership) and lets
you diff state precisely.

### Screenshot naming

Naming is part of the deliverable. Use this format strictly:

```
{NN}-{scenario}-{phase}.png

01-cast-bolt-before.png
02-cast-bolt-after.png
03-cast-bolt-targeted-creature-died.png
04-negative-illegal-target-blocked.png
05-edge-empty-hand-cast-disabled.png
```

`NN` is a two-digit sequence so the gallery sorts correctly.

## Step 4 — Verify state changes

For every state-changing scenario:

1. **Diff the state JSON pair** — if the screenshot looks right but state is
   wrong, the engine is fine and the UI is lying. Report it.
2. **Diff visible counters explicitly** — life totals, mana pool, hand size,
   stack depth. Don't eyeball; record numbers.
3. **Watch for missing animations** — Framer Motion animations should
   complete; if `animationStore` queues never drain, the next action will be
   ignored. Capture an extra "stale animation" screenshot if you see this.

Phase.rs-specific gotchas:

- **WASM async queue.** `WasmAdapter` serializes actions through an async queue.
  Click a button, then wait ~100-300ms before snapshotting again.
- **Multiplayer state filter.** If testing multiplayer, the opponent's hand /
  library MUST appear hidden in your client. Verify explicitly.
- **Replacement effects.** A "destroy" might become "exile" via a replacement;
  trust the engine state, not the button you clicked.

## Step 5 — Compose the evidence table

For each scenario, persist explanations as you go so Step 6 can post them
verbatim. Bash 4+ required (Homebrew bash is fine; macOS default bash is 3.x):

```bash
declare -A SCREENSHOT_EXPLANATIONS=(
  ["01-cast-bolt-before.png"]="Hand shows Lightning Bolt; opposing creature at 3/3."
  ["02-cast-bolt-after.png"]="Bolt resolves; creature exiles to graveyard; player mana pool empty."
  ["04-negative-illegal-target-blocked.png"]="Bolt cast attempt on protection-from-red creature; engine rejects; hand unchanged."
)

TEST_RESULTS_TABLE="| 1 | Cast Bolt on creature | PASS | Life 20→20, opp creature toughness 3→destroyed | 01-, 02- |
| 2 | Negative: protection blocks | PASS | Bolt rejected, hand unchanged | 04- |"
```

## Step 6 — Upload screenshots and post the PR comment

Upload via the GitHub Git API (server-side blob/tree/commit/ref creation — no
local `git checkout` or `git push`, safe in worktrees).

```bash
SCREENSHOTS_BRANCH="test-screenshots/pr-${PR_NUMBER}"
SCREENSHOTS_DIR="test-screenshots/PR-${PR_NUMBER}"

shopt -s nullglob
SCREENSHOT_FILES=("$RESULTS_DIR"/*.png)
if [ ${#SCREENSHOT_FILES[@]} -eq 0 ]; then
  echo "ERROR: No screenshots in $RESULTS_DIR. Test run is incomplete."
  exit 1
fi

# Build blobs (3 retries each) + tree JSON
TREE_JSON='['
FIRST=true
FAILED_UPLOADS=()
for img in "${SCREENSHOT_FILES[@]}"; do
  BASENAME=$(basename "$img")
  # `base64` wraps lines on Linux (76 cols default) which corrupts the API
  # payload; macOS doesn't wrap. `tr -d '\n'` normalizes both platforms.
  B64=$(base64 < "$img" | tr -d '\n')
  BLOB_SHA=""
  for attempt in 1 2 3; do
    BLOB_SHA=$(gh api "repos/${REPO}/git/blobs" \
      -f content="$B64" -f encoding="base64" \
      --jq '.sha' 2>/dev/null || true)
    [ -n "$BLOB_SHA" ] && break
    sleep 1
  done
  if [ -z "$BLOB_SHA" ]; then
    FAILED_UPLOADS+=("$img")
    continue
  fi
  if $FIRST; then FIRST=false; else TREE_JSON+=','; fi
  TREE_JSON+="{\"path\":\"${SCREENSHOTS_DIR}/${BASENAME}\",\"mode\":\"100644\",\"type\":\"blob\",\"sha\":\"${BLOB_SHA}\"}"
done
TREE_JSON+=']'

TREE_SHA=$(echo "$TREE_JSON" | jq -c '{tree: .}' \
  | gh api "repos/${REPO}/git/trees" --input - --jq '.sha')
COMMIT_SHA=$(gh api "repos/${REPO}/git/commits" \
  -f message="test: add E2E test screenshots for PR #${PR_NUMBER}" \
  -f tree="$TREE_SHA" --jq '.sha')
gh api "repos/${REPO}/git/refs" \
  -f ref="refs/heads/${SCREENSHOTS_BRANCH}" \
  -f sha="$COMMIT_SHA" 2>/dev/null \
  || gh api "repos/${REPO}/git/refs/heads/${SCREENSHOTS_BRANCH}" \
       -X PATCH -f sha="$COMMIT_SHA" -f force=true
```

Build the comment body and post it:

```bash
REPO_URL="https://raw.githubusercontent.com/${REPO}/${SCREENSHOTS_BRANCH}"
IMAGE_MARKDOWN=""
for img in "${SCREENSHOT_FILES[@]}"; do
  BASENAME=$(basename "$img")
  TITLE=$(echo "${BASENAME%.png}" | sed 's/^[0-9]*-//' | sed 's/-/ /g' \
    | awk '{for(i=1;i<=NF;i++) $i=toupper(substr($i,1,1)) tolower(substr($i,2))}1')
  IS_FAILED=false
  for failed in "${FAILED_UPLOADS[@]}"; do
    [ "$(basename "$failed")" = "$BASENAME" ] && IS_FAILED=true && break
  done
  $IS_FAILED && continue
  EXPLANATION="${SCREENSHOT_EXPLANATIONS[$BASENAME]}"
  if [ -z "$EXPLANATION" ]; then
    echo "ERROR: Missing screenshot explanation for $BASENAME. Add it in Step 5."
    exit 1
  fi
  IMAGE_MARKDOWN="${IMAGE_MARKDOWN}
### ${TITLE}
![${BASENAME}](${REPO_URL}/${SCREENSHOTS_DIR}/${BASENAME})
${EXPLANATION}
"
done

FAILED_SECTION=""
if [ ${#FAILED_UPLOADS[@]} -gt 0 ]; then
  FAILED_SECTION="
## ⚠️ Failed Screenshot Uploads
These screenshots could not be uploaded via the GitHub API after 3 retries.
Attach them manually in a follow-up PR comment:
"
  for failed in "${FAILED_UPLOADS[@]}"; do
    FAILED_SECTION+="
- \`$(basename "$failed")\` (local path: \`$failed\`)"
  done
  FAILED_SECTION+="

**Run status:** INCOMPLETE until the files above are attached and visible inline."
fi

# BSD mktemp (macOS) requires a template or `-t` flag; GNU mktemp doesn't.
# Use `-t` so the same line works on both platforms.
COMMENT_FILE=$(mktemp -t phase-e2e-comment.XXXXXX)
cat > "$COMMENT_FILE" <<EOF
## E2E Frontend Test Report

| # | Scenario | Result | Engine state evidence | Screenshots |
|---|----------|--------|----------------------|-------------|
${TEST_RESULTS_TABLE}

${IMAGE_MARKDOWN}
${FAILED_SECTION}
EOF

# `-f body=@FILE` reads as string; `-F` would type-infer (JSON literals,
# numbers, etc.) and can mangle markdown content. Always use `-f` for text.
gh api "repos/${REPO}/issues/${PR_NUMBER}/comments" -f body=@"$COMMENT_FILE"
rm -f "$COMMENT_FILE"
```

The comment is the deliverable. If `gh api` fails on the upload or the comment
post, retry once; surface the error if it still fails.

## Step 7 — Cleanup

```bash
agent-browser --session-name phase-e2e close 2>/dev/null || true
# RESULTS_DIR stays — it's gitignored under test-results/ and useful for re-runs.
```

The sidecar branch `test-screenshots/pr-${PR_NUMBER}` stays on the remote. It's
isolated from `main` and doesn't go through the merge queue. Maintainers can
delete it manually after the PR lands.

## Known issues and workarounds

### agent-browser ref selectors are stale after route change

Phase.rs uses React Router. After navigation, the previous page's refs
disappear. Always re-snapshot after `agent-browser open <new-path>`:

```bash
agent-browser --session-name phase-e2e open 'http://localhost:5173/setup' --timeout 10000
agent-browser --session-name phase-e2e snapshot | grep -E "button|combobox"
```

### Vite HMR can interrupt a test mid-scenario

If you edit a file while the test is running, Vite hot-reloads the page and
your `agent-browser` session state may desync. Either pause edits during a run,
or use `--no-hmr` if you've configured it in `vite.config.ts`.

### WASM async queue makes "click then snapshot immediately" return stale state

The `WasmAdapter` serializes engine actions through an async queue. After
clicking an action button, wait 100-300ms before snapshotting / capturing
state. For longer actions (full turn resolution with cascading triggers), wait
longer or, when a debug store global is explicitly exposed, poll
`window.__gameStore?.getState?.().pending` for completion.

### Caddy HTTPS proxy vs raw Vite

If `tilt up --enable=https` is active, the page is served via Caddy at
`https://local.phase-rs.dev`. WebSocket connections expect that origin. Pick
ONE — either drive tests against `http://localhost:5173` (Vite direct) OR
`https://local.phase-rs.dev` (proxy). Don't mix; cookies / WebSocket origins
will diverge.

### Coverage page slow load

`/coverage` parses card-data.json (~10MB+) on mount. Bump the `--timeout` to
30s+ on that page or expect intermittent timeouts.

### Engine action requires user input mid-resolution

Many spells / abilities require choices (target, modal, X value) via the
`WaitingFor` continuation pattern. The UI surfaces these as overlays. Don't
treat them as bugs — drive through them explicitly in the scenario plan:
target → confirm → wait → snapshot.

## Relation to other skills

| Skill | What it does | Use when |
|-------|-------------|----------|
| `/verify` | Launch the app, observe a change works | Quick local check, no PR artifact needed |
| `/run` | Launch the app per the project's launch convention | Just need it running |
| `/e2e-frontend-test` | Drive scenarios, capture before/after, post PR comment | PR needs durable evidence |
| `/pr-contribution-handler` | Full PR shepherd: diff review, comments, merge | Engine/parser PRs, or after this skill posts evidence |
| `agent-browser` (skill) | The actual browser-driving tool | Always — this skill depends on it |
