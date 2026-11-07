---
name: qwen-issue-triage
description: Triage a batch of open phase.rs GitHub issues and group them into clusters that share a root cause. Proposes labels, duplicates and cluster trackers, and writes everything to triage/qwen/ for a maintainer to review and apply. Written for Qwen Code; works in any agent with a shell, gh and jq, or pasted into a chat model.
---

# Issue triage and clustering

phase.rs (`phase-rs/phase`) is a Magic: The Gathering rules engine in Rust with a React frontend. It has over 1,500 open issues. Many have no labels, and roughly 400 are player reports from Discord or the GitHub bug form that nobody has sorted yet. Your job on each run:

1. Take a batch of open issues and decide, for each one, what it is, how bad it is, which part of the codebase owns it, and whether it duplicates something already filed.
2. Group the issues you've triaged into clusters that one fix, or one closely related set of fixes, would resolve.

You produce proposals; you don't edit issues. Most people running this can't label issues on `phase-rs/phase` anyway, so the output is a set of files a maintainer reviews and applies.

Be decisive. A proposal with a clear call and a stated reason is useful even when it's wrong, because the reviewer can see the reasoning and correct it. A proposal that hedges every field is not. Use `confidence` to say how sure you are and keep the rest direct.

## Arguments

The text after the command picks the batch:

- a number, such as `40`: that many issues from the default slice
- `unlabeled`: issues with no labels at all
- `pending`: issues labeled `classifier:pending` (player card reports)
- one or more issue numbers, such as `10507 10529`
- anything else is used as the GitHub search query, such as `label:area:engine Worldgorger`
- `cluster`: skip triage and only rerun clustering over everything already in `triage/qwen/proposals.jsonl`

The default slice is the 30 newest issues labeled `status:needs-triage` that aren't in `triage/qwen/proposals.jsonl` yet.

## Setup

You need `gh` (logged in, `gh auth status`), `jq` and `curl`, run from the repo root. Everything you write goes in `triage/qwen/`, which git ignores:

```bash
mkdir -p triage/qwen
touch triage/qwen/proposals.jsonl
```

Card text and parse data come from the published coverage file. It's about 70 MB, so download it once and reuse it:

```bash
[ -s triage/qwen/coverage-data.json ] || curl -sSfo triage/qwen/coverage-data.json https://data.phase-rs.dev/preview/coverage-data.json
```

## Step 1: Fetch the batch

This walks the search results newest first, a page of 100 at a time, skipping issues already in `proposals.jsonl`, until it has `N` new ones or runs out of results. Proposals don't change live labels, so earlier runs' issues still match the search; filtering before counting is what lets each run pick up where the last one stopped.

```bash
N=30                            # batch size: set from the argument
Q='label:status:needs-triage'   # unlabeled: 'no:label'; pending: 'label:classifier:pending'; a search argument: the argument itself
: > triage/qwen/batch.jsonl
page=1 TOTAL=0 INCOMPLETE=0 STOP=
while :; do
  if [ "$page" -gt 10 ]; then STOP=cap; break; fi
  if ! gh api -X GET search/issues -f q="repo:phase-rs/phase is:issue is:open $Q" \
      -f sort=created -f order=desc -f per_page=100 -f page="$page" > triage/qwen/page.json; then
    STOP="api-error on page $page"; break
  fi
  [ "$(jq .incomplete_results triage/qwen/page.json)" = true ] && INCOMPLETE=1
  TOTAL=$(jq .total_count triage/qwen/page.json)
  jq -c --slurpfile done triage/qwen/proposals.jsonl '
      ($done | map(.number)) as $seen
      | .items[] | select(.number as $n | $seen | index($n) | not)
      | {number, title, labels: [.labels[].name], author: .user.login, createdAt: .created_at, body: ((.body // "")[0:2000])}' \
    triage/qwen/page.json >> triage/qwen/batch.jsonl
  if [ "$(wc -l < triage/qwen/batch.jsonl)" -ge "$N" ]; then STOP=full; break; fi
  if [ "$(jq '.items | length' triage/qwen/page.json)" -lt 100 ] || [ $((page * 100)) -ge "$TOTAL" ]; then
    STOP=exhausted; break
  fi
  page=$((page + 1))
done
head -n "$N" triage/qwen/batch.jsonl > triage/qwen/batch.tmp && mv triage/qwen/batch.tmp triage/qwen/batch.jsonl
[ "$INCOMPLETE" = 1 ] && STOP="$STOP, search incomplete"
echo "$STOP: $(wc -l < triage/qwen/batch.jsonl) issues in batch, $TOTAL matching"
```

The last line says why the walk stopped, and whether GitHub's search was incomplete along the way. Put both in the run report, and triage whatever is in the batch either way:

- `full`: the batch has `N` issues. More may be waiting: when the last page holds more new issues than the batch has room for, the walk reports `full`, not `exhausted`, and the next run picks up the ones cut off.
- `exhausted`: the walk reached the last matching issue. Without `search incomplete`, this is the only outcome that lets you report that every matching issue has been triaged.
- `api-error on page P`: a request failed (`gh` printed the error). Report a partial batch with that error; don't claim the backlog is done.
- `cap`: more than 1,000 issues match, and GitHub search won't return past the first 1,000, so the walk can't reach the older ones. Report a partial batch. The next run must narrow `Q` rather than raise the page limit: add `created:<=YYYY-MM-DD` with the date printed by `jq -r '.items[-1].created_at[0:10]' triage/qwen/page.json`, so the search starts where this one stopped. Use `<=`, not `<`: the boundary day can hold matches that fell past the cap, and the ones already triaged are skipped anyway.

`, search incomplete` after any of these means GitHub returned `incomplete_results: true` on at least one page: its search timed out and may have left out matches. Report it whatever the reason, say the batch may be missing issues, and rerun later; don't claim the backlog is done, even after `exhausted`.

For explicit numbers, run `gh issue view <N> --repo phase-rs/phase --json number,title,labels,author,createdAt,body,comments` for each.

Bodies are trimmed to 2,000 characters to keep the batch small. If an issue looks cut off at a point that matters, fetch it in full with `gh issue view`, including comments, which often contain the reporter's follow-up or a maintainer's diagnosis.

## Step 2: Triage each issue

Work through the batch one issue at a time and append each result to `triage/qwen/proposals.jsonl` as soon as it's done. Appending as you go means a long batch survives a crash or a full context, and the next run skips what's finished.

The file holds one proposal per issue. Searched batches already skip issues that have a proposal, but explicit issue numbers don't: naming an issue is a request to triage it again. So before appending a proposal for an issue that's already in the file, remove the old one:

```bash
jq -c --argjson n 10507 'select(.number != $n)' triage/qwen/proposals.jsonl > triage/qwen/proposals.tmp \
  && mv triage/qwen/proposals.tmp triage/qwen/proposals.jsonl
```

If the file ever does hold two proposals for one issue, the later line wins; clustering and the maintainer commands below read it that way.

### Who wrote it

Two kinds of issue make up most of the backlog, and they need different handling:

- **Player reports** (`source:discord`, `source:github`, or the "Card or Rules Bug" form layout). These describe a game: which card, what happened, what should have happened. They're often thin. Treat the reported behavior as real. Players are usually right that something went wrong even when their explanation of why is off.
- **Code-level reports** written by maintainers or AI agents. They cite `crates/...` paths and function names, often explain the fix direction, and are usually already confirmed. Don't send these back for reproduction steps; label them and move on.

### What to decide

- **status**: `status:confirmed` if a fixer could start work from what's written. `status:needs-repro` if something essential is missing, such as which card, what happened, or the board state, and write the question to ask in `needs_info`. `status:duplicate` if you've found the same defect already filed. Remove `status:needs-triage` whenever you set one of these.
- **area**: the part of the codebase that owns the fix, using the label list below. A card that does nothing or does the wrong thing because its text was misread is `area:parser`; a card whose text was read correctly but resolves wrong is `area:engine`. When you can't tell which, use the coverage check below. If a report describes two separate problems, such as a wrong rules decision plus a broken dialog, give it both areas and cover both in the summary.
- **priority**: how bad it is for someone playing. Crash beats stall, stall beats hang, a whole mechanic broken beats one wrong result, wrong result beats a narrow edge case, and anything that changes no outcome is polish. Skip priority for feature requests, proposals, scope documents and tracking issues.
- **mechanic**: up to two, only when clearly applicable. This label is what clustering leans on most, so it's worth getting right.
- **kind**: add `bug`, `enhancement`, `feature`, `refactor`, `test` or `documentation` only if the issue has no kind label and it's obvious.

### Card reports: the coverage check

When the report names a card, look it up:

```bash
jq -c --arg n "Card Name" 'first(.cards[] | select(.card_name == $n)) | {oracle_text, supported, parse_details}' triage/qwen/coverage-data.json
```

The name is case-sensitive. If nothing comes back, try without apostrophes, the front face of a double-faced card, or the obvious typo fix. If you still can't find it, say so in `notes`; a card missing from the data is worth knowing about in itself.

Quote card text from card data, not from the issue or from memory. Reports regularly paraphrase or misremember the card, and an issue filed against the wrong text sends the fixer after the wrong behavior. If you have a locally generated `client/public/card-data.json` (developer setups do; it's gitignored and built by the card-data pipeline), try it first, since it's the repo's authoritative copy:

```bash
jq -er --arg n "card name in lowercase" '.[$n].oracle_text | strings | select(length > 0)' client/public/card-data.json
```

Use what this prints only if it exits 0. Any other exit means there's no usable text: 2 if the file isn't there, 4 if the card isn't in it or has empty text. In that case quote `oracle_text` from the coverage lookup above instead, and check that it's a non-empty string too. The two files agree for almost every card; they differ only when one was built before an Oracle text update. Record the file you actually quoted from in `oracle_source`. If neither has text for the card, set `oracle_source` to `null`, don't quote any card text, and say in `notes` which lookups failed.

Then work out which clause the report is about. `parse_details` lists each ability with its `source_text`, a `supported` flag, a `label`, and `details` and `children` describing what the parser built. Pick the one clause the complaint matches and set the classifier verdict:

- `unsupported-aspect`: that clause is `supported: false`. It's a known gap; the report is right, but the work is already tracked.
- `supported-aspect-defect`: that clause is `supported: true` and the card still misbehaves. This is the bucket that matters most, so put your effort here. Read the clause's `children` and `details` against its `source_text` and look for what was lost: a sub-effect missing from the chain ("discards a card, and loses 3 life" with no discard child), a target filter that dropped "an opponent controls", a wrong count, the wrong controller or zone. Name the specific gap in `reasoning`. If the parse looks faithful, the verdict is still `supported-aspect-defect`: the card claims support and doesn't deliver it, so the bug is in how the engine runs a correctly parsed ability. Say "parse looks faithful; runtime bug in <where>" in `reasoning`.
- `not-card-data`: the complaint isn't about what a card's ability does. Combat damage assignment, AI choices, UI layout, multiplayer and the deckbuilder fall here. A card ability resolving wrong is never `not-card-data`, even when the fault is in a shared engine module rather than the parser.
- `cannot-determine`: you can't tell which clause is meant. Put the question for the reporter in `needs_info`.

Fill in `classifier` for every card report, but only propose a classifier label when the issue already carries `classifier:pending`: swap it for `classifier:<verdict>`. Issues without `classifier:pending` didn't come through the classifier pipeline, so leave their labels alone and let the verdict live in the proposal.

"Supported" only means the parser produced a typed result. It doesn't mean the result is right, so a supported clause is never evidence that a reported bug is fixed.

### Duplicates

Search before calling something new:

```bash
gh issue list --repo phase-rs/phase --state all --search "\"Card Name\"" --limit 20 --json number,title,state,labels
```

For issues without a card, search two or three distinctive words from the defect. A duplicate means the same defect, not the same card: two reports about one card's different abilities are separate issues. If the earlier issue is closed as fixed and the new report is newer, it's a possible regression, not a duplicate; say so in `notes`.

### Proposal format

One JSON object per line in `triage/qwen/proposals.jsonl`:

```json
{"number": 10529, "title": "Game log names the wrong player on \"passes priority\"", "kind": "engine-bug", "cards": [], "oracle_source": null, "labels_add": ["status:confirmed", "area:engine", "priority:p4-ui-polish", "bug"], "labels_remove": [], "classifier": null, "duplicate_of": [], "needs_info": null, "summary": "PriorityPassed carries the player receiving priority, so the log credits each pass to the wrong player.", "root_cause": "PriorityPassed event payload holds the next player, and log.rs renders it as the passer.", "confidence": "high", "notes": ""}
```

- `kind`: one of `card-bug`, `engine-bug`, `ui-bug`, `ai-bug`, `multiplayer-bug`, `deckbuilder-bug`, `tooling`, `docs`, `feature-request`, `proposal`, `not-a-bug`.
- `oracle_source`: `"card-data.json"` or `"coverage-data.json"` when you quoted card text, otherwise `null`.
- `classifier`: `null`, or `{"verdict": "...", "matched_clause": "<source_text from coverage data>", "supported": true, "reasoning": "..."}`.
- `duplicate_of`: `[{"number": 123, "confidence": "high|medium|low", "why": "..."}]`.
- `summary`: the defect in one line, not a restatement of the title.
- `root_cause`: your best one-line guess at the mechanism, such as "copy effects don't carry cast-time choices" or "trigger fires once per batch instead of once per object". Clustering uses this field, so make it about the mechanism rather than the card. Write `"unknown"` when you can't tell.
- `notes`: anything the reviewer should know. Empty string if nothing.

## Step 3: Cluster

Clustering runs over everything in `triage/qwen/proposals.jsonl`, not just this batch (one proposal per issue, the last line winning), so clusters improve as more issues are triaged. Run it at the end of every batch, or on its own with `cluster`.

Start by loading the clusters that already exist. They're open issues labeled `collector`, titled `Cluster: <theme> (<scope>)`, with their member issues attached as GitHub sub-issues. This fetches each tracker with the issue numbers already attached to it:

```bash
gh api graphql -f query='{ repository(owner: "phase-rs", name: "phase") { issues(first: 50, states: OPEN, labels: ["collector"]) { nodes { number title body subIssues(first: 100) { nodes { number } } } } } }' \
  --jq '.data.repository.issues.nodes[] | {number, title, body: .body[0:800], attached: [.subIssues.nodes[].number]}' \
  > triage/qwen/trackers.jsonl
```

Then group the proposals:

- **Group by mechanism, not by label.** A good cluster is a set of issues that one fix, or one shared piece of new engine support, would resolve: "copy effects drop cast-time choices", "triggers fire once per batch instead of once per object", "attachments lose their host after a zone change". "All trigger bugs" or "all AI issues" is a label search, not a cluster. Start from `root_cause`, then `mechanic`, then the files code-level reports cite.
- **Prefer existing trackers, but only on a mechanism match.** Read the tracker's title and rationale, and attach an issue only if it has the mechanism the tracker describes. Touching the same subsystem isn't enough: a bug about who controls a spell copy doesn't belong in a tracker about copies losing their copiable values, even though both are copy bugs. If the fit rests on the area rather than the mechanism, leave the issue unclustered or start a new cluster.
- **Three or more issues** make a new cluster. Record pairs as `related` in the cluster file without proposing a tracker; they may become a cluster once more issues are triaged.
- **One cluster per issue.** If an issue fits two, pick the cluster whose theme better predicts what the fix looks like.
- **A cluster needs an end.** It should be done when its issues are fixed. "Ongoing parser gaps" never ends, so it isn't a cluster.
- **Open questions:** if a cluster rests on a guess about root cause, say what would confirm it, such as "check whether `apply_copy` reads the stack object's chosen modes".

Write two files, replacing the previous versions:

`triage/qwen/clusters.json`, an array of objects like this one (the issue numbers are made up):

```json
{"title": "Cluster: copy effects drop cast-time choices (Nov 2026 triage)", "existing_tracker": null, "members": [101, 102, 103], "new_members": [101, 102], "conflicts": [{"number": 103, "current_parent": 4800, "why": "Attached to #4800 (combat participation), but its root cause is the copy's chosen modes, which this cluster predicts better."}], "rationale": "All three copy a spell whose modes or X were chosen on cast, and the copy resolves with defaults. One fix in how copies inherit the original's choices covers them.", "order": "103 first; it has the simplest reproduction.", "confidence": "medium", "confirm_by": "Check whether copies read the original stack object's chosen modes."}
```

Set `existing_tracker` to the tracker's issue number when you're proposing to attach members to it, and `null` for a new cluster. `members` is every issue in the cluster.

An issue can have only one parent, and a member may already sit under a tracker that isn't in `trackers.jsonl`: a closed one, one past the first 50, or a parent that isn't labeled `collector`. So once `members` is drafted, look up each member's current parent:

```bash
jq -r '[.[].members[]] | unique
  | map("i\(.): issue(number: \(.)) { number parent { number } }")
  | "{ repository(owner: \"phase-rs\", name: \"phase\") { \(join(" ")) } }"' triage/qwen/clusters.json > triage/qwen/parents.graphql
gh api graphql -f query="$(cat triage/qwen/parents.graphql)" \
  --jq '.data.repository[] | {number, parent: .parent.number}' > triage/qwen/parents.jsonl
```

Then sort each member by what came back:

- `parent` is `null`: it goes in `new_members`.
- `parent` is this cluster's `existing_tracker`: it's already attached; keep it in `members` and leave it out of `new_members`.
- `parent` is any other issue: leave it out of `new_members` and add it to `conflicts` with `current_parent` and a one-line `why` explaining which parent fits better. Never propose moving it; a maintainer decides whether to reparent. If the current parent fits at least as well, drop the issue from this cluster instead.

`conflicts` is an empty list when there are none.

`triage/qwen/clusters.md`, the same clusters for a person to read: one `##` heading per cluster, the rationale, the member issues with their one-line summaries (mark the ones in `new_members`, and list each conflict with its current parent), and the existing tracker if there is one. End with a short `## Related pairs` list and a `## Unclustered` count.

## Step 4: Finish the run

Print a short report:

- how many issues you triaged, with counts by status and by classifier verdict
- how the Step 1 search ended (`full`, `exhausted`, `api-error` or `cap`) and whether it was marked `search incomplete`; for `api-error`, `cap` or anything marked incomplete, that the backlog isn't finished and what the next run should do
- the issues that need a maintainer's eye: likely duplicates, possible regressions, anything that looks like a crash or a stall, and anything you'd close
- clusters that are new or that grew this run, and any members listed under `conflicts`

The files in `triage/qwen/` are the output: `proposals.jsonl`, `clusters.json` and `clusters.md`. Share them wherever the maintainers asked for feedback.

## Labels

Use these and only these. If the label you want doesn't exist, leave that field out and say so in `notes`.

**status**: `status:needs-triage`, `status:confirmed`, `status:needs-repro`, `status:duplicate`. The later lifecycle states (`in-progress`, `fixed-unreleased`, `needs-runtime-verify`, `verified`, `stale`, `wont-fix`) are set by whoever works on the fix.

**area**:
- `area:engine`: rules behavior, resolution, the stack, combat, priority, state-based actions, game log events
- `area:parser`: Oracle text turned into the wrong or a missing typed ability
- `area:card-data`: a card missing from the data, or wrong text or metadata
- `area:oracle-gen`: Oracle text generation and export tooling
- `area:frontend`: React UI, rendering, prompts and selection dialogs
- `area:ai`: AI opponent decisions
- `area:multiplayer`: P2P, server, lobbies, reconnects, seats, 2v2 and Two-Headed Giant setup
- `area:sync`: state sync between clients
- `area:deckbuilder`: deck import, legality, deckbuilder UI
- `area:docs`: documentation, skills, contributor guides

**priority**:
- `priority:p0-panic`: crash, panic or WASM error
- `priority:p0-softlock`: the game can't continue
- `priority:p1-infinite-loop`: the engine hangs or loops forever
- `priority:p1-core-mechanic`: a widely used rule is broken across many cards
- `priority:p2-wrong-game-result`: a card or interaction produces the wrong outcome
- `priority:p3-card-specific`: one card or an unusual edge case
- `priority:p4-ui-polish`: cosmetic; no effect on the game

**mechanic**: `mechanic:costs`, `mechanic:mana`, `mechanic:replacement-effects`, `mechanic:triggers`, `mechanic:continuous-effects`, `mechanic:layers`, `mechanic:attachments`, `mechanic:tokens`, `mechanic:copy`, `mechanic:search`, `mechanic:combat`, `mechanic:zone-change`, `mechanic:keyword`, `mechanic:modal`, `mechanic:counters`, `mechanic:commander`, `mechanic:ai-policy`, `mechanic:ui-selection`

**classifier**: `classifier:pending`, `classifier:supported-aspect-defect`, `classifier:unsupported-aspect`, `classifier:not-card-data`, `classifier:cannot-determine`

**kind**: `bug`, `enhancement`, `feature`, `refactor`, `test`, `documentation`

Leave `source:*`, `resolution:*`, `collector`, `pr:*`, `contributor:*` and the maintainer-workflow labels alone.

## Without tools

If you're a chat model with no shell, the person will paste this file followed by one or more issues. Do Step 2 for each issue and reply with the proposal lines. Skip the coverage lookup and the duplicate search, set `classifier` to `null`, and say in `notes` that card text is unverified. When the person asks you to cluster, they'll paste a set of proposal lines; do Step 3 over those, without existing trackers unless they paste those too. You can't look up parents, so leave `conflicts` empty and say in each cluster's `confirm_by` that current parents weren't checked.

## For maintainers: applying proposals

This prints the label commands for review instead of running them:

```bash
jq -rs 'group_by(.number) | map(last) | .[] | select(.confidence != "low")
  | "gh issue edit \(.number) --repo phase-rs/phase"
    + ([.labels_add[] | " --add-label \(@sh)"] | add // "")
    + ([.labels_remove[] | " --remove-label \(@sh)"] | add // "")' triage/qwen/proposals.jsonl
```

Attaching members to a tracker uses the sub-issues API, which takes the issue's REST id rather than its number. This prints the commands for every `new_members` entry of clusters that target an existing tracker:

```bash
jq -r '.[] | select(.existing_tracker != null) | .existing_tracker as $t | .new_members[]
  | "gh api -X POST repos/phase-rs/phase/issues/\($t)/sub_issues -F sub_issue_id=\"$(gh api repos/phase-rs/phase/issues/\(.) --jq .id)\""' triage/qwen/clusters.json
```

A new cluster has `existing_tracker: null`, so the command above skips it until its tracker exists. Create the tracker, then write its number back into `clusters.json` so the command picks it up. Set `TITLE` to the cluster's `title`:

```bash
TITLE='Cluster: <theme> (<scope>)'
T=$(jq -r --arg title "$TITLE" '.[] | select(.title == $title and .existing_tracker == null)
      | "## Rationale\n\n\(.rationale)\n\n## Ordering\n\n\(.order)\n"' triage/qwen/clusters.json \
    | gh issue create --repo phase-rs/phase --label collector --title "$TITLE" --body-file - \
    | sed 's#.*/##')
jq --argjson t "$T" --arg title "$TITLE" \
   'map(if .title == $title and .existing_tracker == null then .existing_tracker = $t else . end)' \
   triage/qwen/clusters.json > triage/qwen/clusters.tmp && mv triage/qwen/clusters.tmp triage/qwen/clusters.json
```

Then rerun the attach command; it now prints the new tracker's `new_members`.

Members listed under `conflicts` are never in `new_members`, so these commands don't touch them. This prints them for review:

```bash
jq -r '.[] | (.existing_tracker // "new: \(.title)") as $t | .conflicts[]?
  | "#\(.number) is under #\(.current_parent); proposed for \($t): \(.why)"' triage/qwen/clusters.json
```
