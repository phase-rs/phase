---
name: unlock-set
description: Unlock the majority of a Magic set's unsupported cards by clustering missing parser/engine primitives, briefing engine-implementer agents tier-by-tier, and committing between clusters. Use when the user asks to "unlock the [SET] cards", "improve coverage for [SET]", or requests highest-ROI coverage work on a specific set (Standard, Commander precon, supplemental). If no set is named, default to the most recent Standard or Commander set by expected ROI (new-printing density).
---

# Unlock Set Cards — Tier-Based Coverage Pass

Derive a set's unsupported card list, cluster by shared missing primitive (not by card), rank clusters by unlock-count × engineering cost, then run each cluster through `engine-implementer` sequentially. Commit between clusters. Defer anything that would ship a partial runtime worse than Unimplemented.

**Prereqs.** Run from the repo root. `engine-implementer` agent must be available. `cargo`, `jq`, `./scripts/gen-card-data.sh`, and `docs/MagicCompRules.txt` must be present (run `./scripts/fetch-comp-rules.sh` if missing).

---

## Phase 0 — Resolve the Set

If the user named a set, resolve it to a **set code**. Otherwise, pick the highest-ROI recent set using the triage rubric below.

```bash
# By name (fuzzy):
jq -r 'to_entries[] | select(.value.name | test("<user phrase>"; "i")) | "\(.key)\t\(.value.name)\t\(.value.baseSetSize // 0)\t\(.value.type // "")" ' client/public/set-list.json
```

Set codes like `SOC`, `FDN`, `DSK`, `DFT` identify the set. Confirm set code and card count with the user before proceeding if ambiguous; proceed silently if unambiguous.

### Set-Selection Triage (when no set is named)

Rank candidate sets by **format-weighted unlock value**, not raw unsupported count. The project prioritizes Standard and Commander; Modern/Legacy/Vintage are deprioritized because the long tail is enormous and low-leverage.

**Format legality weights** (multiply by unsupported-card count for the set):

| Format scope | Weight | Rationale |
|---|---:|---|
| Standard-legal (also Pioneer/Modern/Commander) | **1.0** | Highest overlap — unlocks cards across every format tier. |
| Commander-legal only (non-Standard, incl. `…Eternal` UB companions, Commander precons) | **0.8** | High value once Draft ships (Play Boosters mix both halves of UB crossovers); Commander is a first-class format here. |
| Modern/Pioneer-only (rotated out of Standard) | **0.4** | Moderate — already covered partially by older passes. |
| Legacy/Vintage-only (Reserved List, old supplemental) | **0.1** | Deprioritized — enormous tail, low leverage. |

**Identifying `…Eternal` UB companions.** Universes Beyond crossovers now ship as *parallel* sets: a Standard-legal base (e.g., `TLA` — Avatar: The Last Airbender) plus a non-Standard `…Eternal` companion (e.g., `TLE` — Avatar: The Last Airbender Eternal) released the same day. Play Boosters mix cards from both. Detection:

```bash
# Find paired base/Eternal sets released together:
jq -r '.[] | select(.name | test(" Eternal$")) | "\(.code)\t\(.name)\t\(.releaseDate)\t\(.baseSetSize)"' client/public/set-list.json
```

When a user says "unlock the Avatar cards," treat `TLA` + `TLE` as a **single bundle** for triage — both appear in the same draft environment and both are Commander-legal. Score the bundle as `(|TLA unsupported| × 1.0) + (|TLE unsupported| × 0.8)`.

**Set-type priority** (from MTGJSON `type` field): `expansion` > `core` > `commander` > `draft_innovation` > `masters` > `funny`/`promo`/`memorabilia` (skip the last three entirely — Un-sets and promos add mechanics we don't want to generalize from).

**Triage procedure.**
1. List candidate sets released in the last ~18 months (or all sets the user scopes to).
2. For each, compute `unsupported_count = |set cards| − |supported set cards|` via the Phase 1/2 pipeline.
3. Multiply by the format weight above. For UB crossovers, sum the base + Eternal bundle.
4. Present the top 3 candidates to the user with weighted scores and a one-line justification each; let them pick.

This prevents defaulting to a raw unsupported-count leader that turns out to be a Legacy-only supplemental product.

---

## Phase 1 — Derive the Set's Card List

Pull every unique card printed in the set from MTGJSON's atomic database (filter by `printings` array):

```bash
jq -r --arg code "<SET_CODE>" \
  '.data | to_entries[] | .value[0] as $c | select(($c.printings // []) | index($code)) | $c.name' \
  data/mtgjson/AtomicCards.json | sort -u > /tmp/set_names.txt
wc -l /tmp/set_names.txt
```

This is the universe of cards to consider.

---

## Phase 2 — Gap Analysis (Typed, not Opaque)

**Do not** treat unsupported cards as opaquely broken. `client/public/coverage-data.json` has per-card `parse_details` with typed failure labels (`trigger:Phase`, `ability:unknown`, etc.) and `gap_bundles` ranking handler-combinations by format-wide unlock count. Use them — **but be aware `parse_details` is only half the signal** (see Phase 2.5).

```bash
# Intersect the set list with the unsupported-card set:
jq -r '.cards[] | select(.supported==false) | .card_name' client/public/coverage-data.json | sort -u > /tmp/all_unsupported.txt
comm -12 <(sort /tmp/set_names.txt) <(sort -u /tmp/all_unsupported.txt) > /tmp/set_unsupported.txt

# Extract typed gap reasons per set target:
jq -r --slurpfile names <(jq -R . /tmp/set_unsupported.txt | jq -s .) '
  .cards[] | select(.card_name as $n | $names[0] | index($n)) |
  [.card_name, ((.parse_details // []) | map(select(.supported==false)) | map("\(.category):\(.label)") | join(" | "))] | @tsv
' client/public/coverage-data.json > /tmp/set_gaps.tsv

# Dump oracle text for clustering by pattern:
while IFS= read -r name; do
  key=$(echo "$name" | tr '[:upper:]' '[:lower:]')
  text=$(jq -r --arg k "$key" '.[$k].oracle_text // "(missing)"' client/public/card-data.json)
  printf "=== %s ===\n%s\n\n" "$name" "$text"
done < /tmp/set_unsupported.txt > /tmp/set_oracles.txt
```

You now have: (a) the set's unsupported cards, (b) their **parser-side** failure categories, (c) their full Oracle text.

### ⚠️ `supported: false` + `gap_count: 0` = hidden gap

**Any card where `supported: false` AND `gap_count: 0` AND `gap_details` is empty/null is invisible to the above pipeline.** These cards parse cleanly but fail one of the non-`parse_details` checks (resolver feature, silent drop, target-fallback warning, subtype lexicon). If you skip Phase 2.5, you will cluster the *parser* gaps correctly and then spawn an `engine-implementer` to fix a parser that is already fine — wasting a review cycle. Count these cards BEFORE proceeding:

```bash
jq --slurpfile names <(jq -R . /tmp/set_unsupported.txt | jq -s .) \
   '[.cards[] | select(.card_name as $n | $names[0] | index($n))
              | select(.supported==false and .gap_count==0)] | length' \
   client/public/coverage-data.json
```

If the count is >0, Phase 2.5 is mandatory for those cards.

---

## Phase 2.5 — Resolver Audit + AST Walk (Mandatory When Phase 2 Signal Is Partial)

Parser gaps are only one failure path. `crates/engine/src/game/coverage.rs::compute_card_coverage` also runs:

- `check_resolver_features` → `ResolverFeature:X` for AST nodes the resolver classifies `Unhandled` (coverage.rs ~line 3640+ Handled/Unhandled tables).
- `check_silent_drops` → Oracle text consumed without emitting a parse item.
- `check_parse_warnings` → target-fallback degradation (`TargetFilter::Any` instead of a specific filter).
- `check_subtype_lexicon` → `AddSubtype` strings that aren't real subtypes (e.g. parser emits `"Horror enchantment creature"` as a single subtype string).

None of these append to the per-card `gap_details` array. They're only visible via the resolver audit and direct AST inspection.

### 2.5a — Resolver audit cross-reference

```bash
# The --audit flag prints the Unhandled feature map + flagged cards to stderr.
cargo run --quiet --bin coverage-report -- data --audit 2>/tmp/resolver_audit.txt 1>/dev/null

# Unhandled feature list (what resolver classifications fail):
sed -n '/Unhandled features/,/Top handled features/p' /tmp/resolver_audit.txt

# Flagged cards for the target set:
grep -A 10000 "^Flagged cards" /tmp/resolver_audit.txt | \
  grep -iFf /tmp/set_unsupported.txt > /tmp/set_resolver_flags.txt
cat /tmp/set_resolver_flags.txt
```

**Note.** The resolver audit currently prints only the first 20 flagged cards by default (coverage_report.rs `.take(20)`). If your set has >20 hidden-gap cards, either (a) bump the take limit temporarily, or (b) skip to the AST walk — it catches the same cards without needing a code patch.

### 2.5b — AST walk for unhandled markers

Walk every unsupported card's full parse tree looking for the specific AST tags that the resolver classifies as `Unhandled`. Bucket cards by their sorted tuple of unhandled features — identical tuples = same cluster.

```python
#!/usr/bin/env python3
# /tmp/ast_cluster.py — run from repo root
import json
data = json.load(open('client/public/card-data.json'))
unsup = [l.strip() for l in open('/tmp/set_unsupported.txt') if l.strip()]

# Pull these sets from the current coverage.rs Unhandled classification tables
# (AbilityCondition, StaticCondition, QuantityRef arms marked Unhandled).
# Refresh these lists when coverage.rs changes.
UNH_ABILITY_COND = {"And","Or","Not","SourceMatchesFilter","TargetMatchesFilter",
                    "ZoneChangedThisWay","SourceIsTapped"}
UNH_STATIC_COND  = {"Not","DefendingPlayerControls","SourceIsEquipped","UnlessPay",
                    "HasMaxSpeed","SourceIsMonstrous","SourceInZone",
                    "OpponentPoisonAtLeast","ControlsCommander","CompletedADungeon",
                    "SourceIsAttacking","SourceAttachedToCreature",
                    "SourceAttackingAlone","IsMonarch","SourceMatchesFilter",
                    "SpeedGE","Unrecognized"}
UNH_QTY_REFS     = {"CreaturesDiedThisTurn","TargetZoneCardCount","AttackedThisTurn",
                    "OpponentHandSize","StartingLifeTotal","OpponentLifeTotal",
                    "DistinctCardTypesExiledBySource"}

def walk(o):
    if isinstance(o, dict):
        yield o
        for v in o.values(): yield from walk(v)
    elif isinstance(o, list):
        for v in o: yield from walk(v)

def features(name):
    card = data.get(name.lower())
    if not card: return ["(card-data missing)"]
    feats = []
    for node in walk(card):
        t = node.get("type")
        if t == "Unimplemented":
            feats.append(f"unimpl:{node.get('name','?')}")
        if t in UNH_ABILITY_COND and ("conditions" in node or "filter" in node):
            feats.append(f"ability_cond:{t}")
        if t in UNH_QTY_REFS:
            feats.append(f"qty_ref:{t}")
    # Trigger mode Unknown(text):
    for trig in (card.get("triggers") or []):
        m = trig.get("mode")
        if isinstance(m, dict) and "Unknown" in m:
            feats.append(f"trigger:Unknown:{m['Unknown']}")
    # Static condition Unrecognized(text) and other unhandled StaticCondition tags:
    for stat in (card.get("static_abilities") or []):
        cond = stat.get("condition")
        if isinstance(cond, dict):
            ct = cond.get("type")
            if ct == "Unrecognized":
                feats.append(f"static_cond:Unrecognized:{cond.get('text','')}")
            elif ct in UNH_STATIC_COND:
                feats.append(f"static_cond:{ct}")
    return sorted(set(feats))

buckets = {}
for n in unsup:
    key = tuple(features(n)) or ("(no-unhandled-marker)",)
    buckets.setdefault(key, []).append(n)

for feats, cards in sorted(buckets.items(), key=lambda x: -len(x[1])):
    print(f"\n[{len(cards)}] {feats}")
    for c in cards: print(f"  - {c}")
```

Run `python3 /tmp/ast_cluster.py`. The output is the cluster table — cards with identical feature tuples share a primitive. Use this as the ground truth for Phase 3 clustering; treat the Phase 2 parse-details output as a sanity cross-check, not as the primary signal.

### 2.5c — Cards with NO unhandled marker

If a card is in the `(no-unhandled-marker)` bucket but still `supported: false`, it's either (a) caught by `check_silent_drops` (rare), (b) caught by `check_subtype_lexicon` — check `AddSubtype` values for multi-word concatenations (`"Horror enchantment creature"` is a misparse that should decompose into multiple `AddType`/`AddSubtype` modifications), or (c) its static ability has an unhandled `condition: {type: "Unrecognized", text}` that my walk missed. Inspect the card's JSON manually.

---

## Phase 3 — Cluster by Shared Primitive

**The single most important step.** Every unlock is a missing *primitive* — a typed enum variant, filter prop, quantity ref, replacement condition, trigger mode, keyword, effect — shared across multiple cards. Group cards by the primitive they need, not by the card they are.

For each cluster, name:
- The primitive to add or extend (e.g., "`TriggerDefinition.origin_zones: Vec<Zone>` for disjunctive-source batched zone-change triggers").
- The set cards it unlocks (count).
- The cross-set cards it likely also unlocks (estimate from grep or by pattern).

**If a cluster covers only one card, something is wrong** — either the pattern hasn't been generalized (fix the clustering), or the mechanic is a genuine one-off (accept, place in Tier 3).

### Tier Assignment

- **Tier 1** — Smallest engine surface × highest unlock count. Usually one typed enum arm, one parser combinator branch, or one matcher-filter call. Expect 5–20 cards flipped per cluster including cross-set. Example: disjunctive-source `origin_zones`, `FilterProp::HasXInManaCost`, widening sacrifice controller scope to `Option<ControllerRef>`.
- **Tier 2** — Medium surface. Requires a new primitive but composes with existing infra. Expect 2–5 cards/cluster. Example: `AttachmentSnapshot` on `ZoneChangeRecord` (CR 603.10a compliance), `QuantityExpr` arithmetic routing for ETB counters, modal-on-dies trigger.
- **Tier 3** — One-off mechanics or narrow keywords. 1–3 cards usually, but occasionally a class keyword (kicker, flashback, ascend, manifest) unlocks ~15–30 cards across MTG history. Still worth doing *if the infrastructure already exists* and you're only missing composition.

Order of execution: Tier 1 → Tier 2 → Tier 3. Within each tier, order by unlock count.

---

## Phase 4 — Present the Plan

Before spawning any agents, present the tier table to the user with:
- Cluster name
- Primitive being added/extended
- Set cards flipped
- Cross-set cards estimated
- Engineering notes (file paths touched, CR sections involved)
- Any clusters flagged for deferral (see Phase 6 criteria)

Wait for the user to confirm the order (or adjust). Then proceed.

---

## Phase 5 — Execute Clusters Sequentially

For each cluster in priority order:

1. **Spawn `engine-implementer`** (not `general-purpose`, not `feature-architect`). This agent runs plan → implement → review internally (per `feedback_engine_implementer_runs_review`), so do not spawn an external reviewer after it finishes.
2. **Brief the agent with the template below.** Under-briefed agents produce inconsistent work.
3. **After the agent returns**, verify the commit exists (`git log -1`) and that tests pass. The agent is responsible for running `cargo fmt / clippy-strict / test -p engine / coverage` before committing.
4. **Commit between clusters** is the agent's responsibility per the brief. Do not amend prior commits.
5. **Handle deferrals** per Phase 6 if the agent returns with a deferral recommendation.

### Working Tree & Isolation

Default to **main without worktree isolation** unless the user asks for worktrees. Per `feedback_engine_implementer_worktree`, ask the user once at the top of the run, then carry that answer through all clusters.

### Multi-Agent Safety

Before briefing each cluster, check `git status`. If there are uncommitted files from other agents outside the current cluster's scope, include in the brief: "The working tree contains uncommitted changes from other agents; do NOT touch those files (multi-agent safety)." Never `git stash`, never `git checkout` to "clean up."

### Agent Brief Template

Every `engine-implementer` invocation must include these sections. Brevity is fine for sections when the information is obvious, but do not omit sections.

```
Context
-------
Unlocking <SET> cards. Tier <N> cluster <M> of <TOTAL>. Work on `main`, no worktree.
Prior work committed through <SHA>. Do not touch files outside this task's scope.

Goal
----
<One-paragraph statement of the primitive to add or extend.>
Target set cards:
  1. <Card name> — <Oracle line being unlocked>
  2. ...
Cross-set impact estimate: <count> cards in <card class>.

Class decomposition
-------------------
<Decompose the Oracle text into typed AST concepts. Identify which pieces are already
supported, which are missing, and which would exceed single-cluster scope.>

Investigation order
-------------------
1. Read <file path 1> — <what to learn>
2. Read <file path 2> — <what to learn>
3. Grep for <analogous primitive> — <why>
4. Read CR <section> via `grep -n "^702\.N" docs/MagicCompRules.txt`
...

**Critical**: trace analogous existing patterns before writing any code.
Half the clusters in prior runs found the primitive was already live and only needed
composition. Do not duplicate.

Design requirements (non-negotiable)
------------------------------------
- Nom combinators on the first pass. No `find()` / `split_once()` / `contains()` /
  `starts_with()` for parsing dispatch.
- No bool flags. Use typed enums (`ControllerRef`, `Comparator`, `Option<T>`) or
  extend existing enums.
- Build for the class, not the card. Every new variant must cover a pattern family.
- Single-authority for costs. Callers dispatch activation; never inspect cost shapes.
- CR annotations: every new rules-touching line needs a CR number verified by
  `grep -n "^XXX\.Y" docs/MagicCompRules.txt` BEFORE writing. Do not rely on memory
  for CR numbers — the 701.x and 702.x tables are arbitrary and easy to misremember
  (e.g., manifest is CR 701.40 not 701.33).

Deliverables
------------
1. Parser: <specific expected AST shape for each target card>.
2. Runtime: <specific observable behavior, including edge cases>.
3. Tests:
   - Parser: <class-level tests, not per-card>.
   - Runtime: <scenarios covering the primitive's full input range>.
4. CR annotations grep-verified.
5. Verification gate:
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test -p engine`
   - `cargo coverage`
   - `cargo semantic-audit` (check no new findings for the target cards).
6. Commit message: `feat(engine): <SET> Tier <N>.<M> — <one-line primitive>\n\n<body>`.
   Do not amend prior commits.

Return format
-------------
Concise summary:
- Variant/field added or extended (show the shape).
- Whether the primitive already existed (and you extended) vs. was new.
- Test names.
- Coverage delta — list ALL newly-supported cards (set + cross-set).
- Commit SHA.
- Any pieces deferred and why.
```

---

## Phase 6 — Defer, Don't Half-Ship

If the agent reports that a cluster needs substantial novel infrastructure (new counter type + new alt-cast pipeline + new duration variant + new legal-action generator), **defer to a dedicated infrastructure cluster**. Partial shipping is worse than Unimplemented because cards appear working but silently misbehave.

Defer criteria (any one is sufficient):
- Needs ≥3 new typed primitives across unrelated subsystems.
- Needs a WaitingFor / GameAction round-trip that doesn't exist yet.
- Partial runtime would cause ≥5 cards to silently resolve incorrectly.
- Parallels a prior deferral (e.g., Theme D/E/F casting-permission durations, Suspend Aggression).

### Memory Capture for Deferrals

When deferring, **write a memory file** to capture the deferral for future sessions:

```markdown
---
name: <deferral name>
description: <one-line what and why>
type: project
---

<Why deferred (reference prior deferral patterns if applicable).>
<What would ship cleanly as a dedicated cluster (proposed scope, 3–4 commits).>
<Which cards/classes unblock when the deferral clears.>
<Date deferred.>
```

Add an index line to `MEMORY.md`.

---

## Phase 7 — Final Report

At the end of the run, summarize:
- Tier counts shipped / deferred.
- Commit SHAs in order.
- Every card flipped (set targets + cross-set bonuses).
- Every SOC target partial (what remains).
- Every deferral with its named Theme / cluster and memory pointer.

This summary is the handoff for the next session.

---

## Recurring Lessons (Pattern Library)

These are the findings that repeat. Brief agents to look for them first.

| Pattern | What it looks like | Where to trace |
|---|---|---|
| Primitive already exists | "The X type was already in the codebase; only the parser dispatch was missing" | Grep the type name, check `coverage.rs` Handled/Unhandled tables |
| Matcher is a pass-through | Trigger/event matcher accepts any event without filter evaluation | `game/trigger_matchers.rs` — check every `match_*` for `valid_card_matches` + `valid_player_matches` |
| Active-zone opt-in missing | Ability parses but doesn't fire from graveyard/exile | `TriggerDefinition.trigger_zones`, `StaticDefinition.active_zones` — auto-detect from effect body |
| Anaphor resolution missing | "those tokens" / "the token created this way" → Unimplemented | `TargetFilter::LastCreated` + `state.last_created_token_ids` + parser post-pass |
| Look-back snapshot missing | "each Aura you controlled that was attached to it" returns 0 | `ZoneChangeRecord` needs a pre-SBA snapshot of relevant state (attachments, counters, types, keywords) |
| Latent merge bug | A new trigger-level condition overwrites an intervening-if instead of And-composing | Grep for `def.condition = ...`; should be `condition.or(def.condition.take())` or And-merge |
| Number word not parsed | "ten or more +1/+1 counters" treated as Unrecognized | `oracle_nom/primitives.rs::parse_english_number` should cover one–twenty; extend if missing |
| CR number hallucinated | Code annotated with CR 701.33 for manifest (it's 701.40) | Grep `docs/MagicCompRules.txt` BEFORE writing any CR annotation |
| Parser double-emits identical condition | `AbilityCondition::And { conditions: [X, X] }` with structurally identical children | Ability-word prefix seeds a condition AND the rephrased "If …" clause re-emits it. Dedup on merge: if a newly-parsed condition is structurally equal to an existing one, return existing rather than wrap in And. Delirium spells are the canonical case. |
| Hidden-gap card: `supported:false`, `gap_count:0` | Card parses cleanly but `check_resolver_features` / `check_silent_drops` / `check_parse_warnings` / `check_subtype_lexicon` flagged it in `missing` without populating `gap_details` | Use Phase 2.5 resolver audit + AST walk. The parse_details-only view misses these entirely. |
| Multi-word subtype misparse | `AddSubtype: "Horror enchantment creature"` (single string of three words) | Parser should decompose "becomes a X Y creature" into `AddType::Enchantment, AddType::Creature, AddSubtype::Horror`. `check_subtype_lexicon` catches these. |
| StaticCondition::Unrecognized leak | `{type: "Unrecognized", text: "..."}` instead of a typed variant | A recurring Oracle phrase got stored verbatim instead of canonicalized to a typed `StaticCondition` arm. Add the typed variant + classify as Handled. |
| TriggerMode::Unknown leak | `{Unknown: "Whenever you fully unlock a Room"}` | Parser fell back. Map the phrase to a typed `TriggerMode` arm; verify a matcher + event emission site exist before considering the trigger functional. A typed arm without a matcher is worse than Unknown because the coverage classifier marks the card supported. |

---

## Skill Invocation

User phrases that trigger this skill:
- "Unlock the [SET] cards"
- "Improve coverage for [SET]"
- "Look into the [SET] cards. How can we unlock the majority?"
- "Work through the unsupported [SET] cards"
- "Highest-ROI coverage pass on [SET]"

If the user does not name a set, propose the most recent Standard expansion or a Commander precon bundle that has the largest unsupported-card count — rank candidates by `|set cards| - |supported set cards|`.
