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
- anything else is passed to `gh issue list --search`, such as `label:area:engine Worldgorger`
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

```bash
gh issue list --repo phase-rs/phase --state open --search "label:status:needs-triage sort:created-desc" --limit 60 \
  --json number,title,labels,author,createdAt,body \
  | jq -c --slurpfile done triage/qwen/proposals.jsonl \
      '[.[] | select(.number as $n | $done | map(.number) | index($n) | not)] | .[0:30][] | .body |= .[0:2000]' \
  > triage/qwen/batch.jsonl
```

The `30` in `.[0:30]` is the batch size; change it to match the argument.

For `unlabeled`, use `--search "no:label sort:created-desc"`. For `pending`, use `--search "label:classifier:pending sort:created-desc"`. For explicit numbers, run `gh issue view <N> --repo phase-rs/phase --json number,title,labels,author,createdAt,body,comments` for each.

Bodies are trimmed to 2,000 characters to keep the batch small. If an issue looks cut off at a point that matters, fetch it in full with `gh issue view`, including comments, which often contain the reporter's follow-up or a maintainer's diagnosis.

## Step 2: Triage each issue

Work through the batch one issue at a time and append each result to `triage/qwen/proposals.jsonl` as soon as it's done. Appending as you go means a long batch survives a crash or a full context, and the next run skips what's finished.

### Who wrote it

Two kinds of issue make up most of the backlog, and they need different handling:

- **Player reports** (`source:discord`, `source:github`, or the "Card or Rules Bug" form layout). These describe a game: which card, what happened, what should have happened. They're often thin. Treat the reported behavior as real. Players are usually right that something went wrong even when their explanation of why is off.
- **Code-level reports** written by maintainers or AI agents. They cite `crates/...` paths and function names, often explain the fix direction, and are usually already confirmed. Don't send these back for reproduction steps; label them and move on.

### What to decide

- **status**: `status:confirmed` if a fixer could start work from what's written. `status:needs-repro` if something essential is missing, such as which card, what happened, or the board state, and write the question to ask in `needs_info`. `status:duplicate` if you've found the same defect already filed. Remove `status:needs-triage` whenever you set one of these.
- **area**: the part of the codebase that owns the fix, using the label list below. A card that does nothing or does the wrong thing because its text was misread is `area:parser`; a card whose text was read correctly but resolves wrong is `area:engine`. When you can't tell which, use the coverage check below.
- **priority**: how bad it is for someone playing. Crash beats stall, stall beats hang, a whole mechanic broken beats one wrong result, wrong result beats a narrow edge case, and anything that changes no outcome is polish. Skip priority for proposals, scope documents and tracking issues.
- **mechanic**: up to two, only when clearly applicable. This label is what clustering leans on most, so it's worth getting right.
- **kind**: add `bug`, `enhancement`, `feature`, `refactor`, `test` or `documentation` only if the issue has no kind label and it's obvious.

### Card reports: the coverage check

When the report names a card, look it up:

```bash
jq -c --arg n "Card Name" 'first(.cards[] | select(.card_name == $n)) | {oracle_text, supported, parse_details}' triage/qwen/coverage-data.json
```

The name is case-sensitive. If nothing comes back, try without apostrophes, the front face of a double-faced card, or the obvious typo fix. If you still can't find it, say so in `notes`; a card missing from the data is worth knowing about in itself.

Quote card text from card data, not from the issue or from memory. Reports regularly paraphrase or misremember the card, and an issue filed against the wrong text sends the fixer after the wrong behavior. If you have a locally generated `client/public/card-data.json` (developer setups do; it's gitignored and built by the card-data pipeline), quote from that, since it's the repo's authoritative copy:

```bash
jq -r --arg n "card name in lowercase" '.[$n].oracle_text' client/public/card-data.json
```

Otherwise quote `oracle_text` from the coverage file. The two agree for almost every card; they differ only when one was built before an Oracle text update. Record which one you used in `oracle_source`.

Then work out which clause the report is about. `parse_details` lists each ability with its `source_text`, a `supported` flag, a `label`, and `details` and `children` describing what the parser built. Pick the one clause the complaint matches and set the classifier verdict:

- `unsupported-aspect`: that clause is `supported: false`. It's a known gap; the report is right, but the work is already tracked.
- `supported-aspect-defect`: that clause is `supported: true` and the engine still misbehaves. This is the bucket that matters most, so put your effort here. Read the clause's `children` and `details` against its `source_text` and look for what was lost: a sub-effect missing from the chain ("discards a card, and loses 3 life" with no discard child), a target filter that dropped "an opponent controls", a wrong count, the wrong controller or zone. Name the specific gap in `reasoning`. If the parse looks faithful, the bug is probably at runtime; say that instead.
- `not-card-data`: the complaint isn't about card text at all. Combat assignment, AI choices, UI, multiplayer and the deckbuilder fall here.
- `cannot-determine`: you can't tell which clause is meant. Put the question for the reporter in `needs_info`.

Propose swapping `classifier:pending` for `classifier:<verdict>`. Note that "supported" only means the parser produced a typed result. It doesn't mean the result is right, so a supported clause is never evidence that a reported bug is fixed.

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

Clustering runs over everything in `triage/qwen/proposals.jsonl`, not just this batch, so clusters improve as more issues are triaged. Run it at the end of every batch, or on its own with `cluster`.

Start by loading the clusters that already exist. They're open issues labeled `collector`, titled `Cluster: <theme> (<scope>)`, with their member issues attached as GitHub sub-issues. This fetches each tracker with the issue numbers already attached to it:

```bash
gh api graphql -f query='{ repository(owner: "phase-rs", name: "phase") { issues(first: 50, states: OPEN, labels: ["collector"]) { nodes { number title body subIssues(first: 100) { nodes { number } } } } } }' \
  --jq '.data.repository.issues.nodes[] | {number, title, body: .body[0:800], attached: [.subIssues.nodes[].number]}' \
  > triage/qwen/trackers.jsonl
```

Then group the proposals:

- **Group by mechanism, not by label.** A good cluster is a set of issues that one fix, or one shared piece of new engine support, would resolve: "copy effects drop cast-time choices", "triggers fire once per batch instead of once per object", "attachments lose their host after a zone change". "All trigger bugs" or "all AI issues" is a label search, not a cluster. Start from `root_cause`, then `mechanic`, then the files code-level reports cite.
- **Prefer existing trackers.** If an issue fits an open tracker's theme, propose attaching it there instead of starting a new cluster.
- **Three or more issues** make a new cluster. Record pairs as `related` in the cluster file without proposing a tracker; they may become a cluster once more issues are triaged.
- **One cluster per issue.** If an issue fits two, pick the cluster whose theme better predicts what the fix looks like.
- **A cluster needs an end.** It should be done when its issues are fixed. "Ongoing parser gaps" never ends, so it isn't a cluster.
- **Open questions:** if a cluster rests on a guess about root cause, say what would confirm it, such as "check whether `apply_copy` reads the stack object's chosen modes".

Write two files, replacing the previous versions:

`triage/qwen/clusters.json`, an array of objects like this one (the issue numbers are made up):

```json
{"title": "Cluster: copy effects drop cast-time choices (Nov 2026 triage)", "existing_tracker": null, "members": [101, 102, 103], "new_members": [101, 102, 103], "rationale": "All three copy a spell whose modes or X were chosen on cast, and the copy resolves with defaults. One fix in how copies inherit the original's choices covers them.", "order": "103 first; it has the simplest reproduction.", "confidence": "medium", "confirm_by": "Check whether copies read the original stack object's chosen modes."}
```

Set `existing_tracker` to the tracker's issue number when you're proposing to attach members to it, and `null` for a new cluster. `members` is every issue in the cluster. `new_members` is the ones that still need attaching: for an existing tracker, leave out anything already in its `attached` list; for a new cluster, it's the same as `members`.

`triage/qwen/clusters.md`, the same clusters for a person to read: one `##` heading per cluster, the rationale, the member issues with their one-line summaries (mark the ones in `new_members`), and the existing tracker if there is one. End with a short `## Related pairs` list and a `## Unclustered` count.

## Step 4: Finish the run

Print a short report:

- how many issues you triaged, with counts by status and by classifier verdict
- the issues that need a maintainer's eye: likely duplicates, possible regressions, anything that looks like a crash or a stall, and anything you'd close
- clusters that are new or that grew this run

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

If you're a chat model with no shell, the person will paste this file followed by one or more issues. Do Step 2 for each issue and reply with the proposal lines. Skip the coverage lookup and the duplicate search, set `classifier` to `null`, and say in `notes` that card text is unverified. When the person asks you to cluster, they'll paste a set of proposal lines; do Step 3 over those, without existing trackers unless they paste those too.

## For maintainers: applying proposals

This prints the label commands for review instead of running them:

```bash
jq -r 'select(.confidence != "low")
  | "gh issue edit \(.number) --repo phase-rs/phase"
    + ([.labels_add[] | " --add-label \(@sh)"] | add // "")
    + ([.labels_remove[] | " --remove-label \(@sh)"] | add // "")' triage/qwen/proposals.jsonl
```

Attaching members to a tracker uses the sub-issues API, which takes the issue's REST id rather than its number. This prints the commands for every `new_members` entry of clusters that target an existing tracker:

```bash
jq -r '.[] | select(.existing_tracker != null) | .existing_tracker as $t | .new_members[]
  | "gh api -X POST repos/phase-rs/phase/issues/\($t)/sub_issues -F sub_issue_id=\"$(gh api repos/phase-rs/phase/issues/\(.) --jq .id)\""' triage/qwen/clusters.json
```

New clusters need their tracker issue created first (`gh issue create --label collector`), then the same command with the new tracker's number.
