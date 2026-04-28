---
name: parser-velocity
description: Fast-iteration loop for quick parser wins — surface near-miss cards via parser-gap-analyzer's typed category classifier, edit in batches, compile once per batch (not per card), defer the full gate (fmt/clippy/test-all/coverage/semantic-audit) to session end. Companion to `unlock-set`; use this when the fix is "add a `tag()` arm to an existing `alt()`" rather than cluster-level infrastructure. Trigger phrases: "quick parser wins", "scan near-misses", "velocity pass on <format>", "parser velocity sprint", "low-hanging parser fruit".
---

# Parser Velocity — Quick-Win Iteration Loop

`unlock-set` runs full gates (`cargo fmt` / `clippy-strict` / `test -p engine` / `coverage` / `semantic-audit`) between every cluster. That's right for cluster-level infrastructure but fatal for near-miss work where the real fix is "add one `tag()` arm to an existing `alt()`." This skill keeps the inner loop fast by batching edits per compile cycle, then running the full gate exactly **once** at session end.

**When to use this skill vs. `unlock-set`:**
- **Use this skill** when the target is "cards that are almost supported" — the parser recognizes most of the text but misses one variation. Category A (VerbVariation), B (SubjectStripping), D (StaticCondition), and parser-miss C (TriggerEffect) cards.
- **Use `unlock-set`** when the target requires a new typed primitive, CR-grounded infrastructure, new runtime mechanic, or anything that warrants plan→implement→review per cluster. Category F (NewMechanic) lives there.

**Prerequisite:** The Phase 1 jq pipeline uses `--rawfile`, which requires **jq ≥ 1.6**. Confirm once per machine: `jq --version`. macOS system jq is sometimes 1.5 — install a newer version via Homebrew if needed.

---

## Phase 0 — Once per session: prepare state

First run after the session starts. Expect this to take **60–120s cold** because `gen-card-data.sh` re-parses the full ~30k-card corpus.

```bash
: > /tmp/velocity-flipped.txt                                 # truncate-or-create empty exclude file
cp client/public/card-data.json /tmp/card-data-before.json    # baseline for regression check at gate
./scripts/gen-card-data.sh                                    # fresh card-data.json for analyzer
```

**The exclude set** lives at `/tmp/velocity-flipped.txt` as plain text, one card name per line, no JSON. It's the session-scoped subtraction filter that prevents already-flipped cards from re-appearing in Phase 1 (because `card-data.json` won't be regenerated until the gate). Populated by Phase 2 step 6. Cleared at the gate.

**Side-effects to expect.** `gen-card-data.sh` writes `client/public/{card-data,card-names,coverage-data,coverage-summary,card-data-meta,set-list,decks}.json`. These files will show modifications throughout the sprint — they are not work product to commit mid-sprint.

---

## Phase 1 — Per batch: detect candidates, subtract exclude set (~2–5s after compile)

Runs at the start of each batch. Does **not** re-run `gen-card-data.sh` — per-batch regeneration would be 60–120s each and destroy velocity. The analyzer will re-surface already-flipped cards from earlier in this session because `card-data.json` is stale; the exclude set filters them out.

Default format is Standard. User can override (e.g., `--format commander`).

```bash
cargo run --profile tool --bin parser-gap-analyzer -- data/ \
  --near-misses-only --format standard \
  | jq -r --rawfile excluded /tmp/velocity-flipped.txt '
      ($excluded | split("\n") | map(select(length > 0))) as $ex |
      .quick_wins
      | map(select(.category | test("^[ABCD]_")))
      | sort_by(-.cards_unlocked)
      | [.[].affected_cards[]]
      | map(select(. as $c | $ex | index($c) | not))
      | .[]' \
  | awk '!seen[$0]++' | head -10 > /tmp/batch.txt
```

**jq mechanics — don't regress these:**
- **Category regex matches the serialized label, not the enum variant name.** `GapCategory::label()` emits `A_verb_variation`, `B_subject_stripping`, `C_trigger_effect`, `D_static_condition`, `F_new_mechanic`, `G_unclassified`. The regex `^[ABCD]_` includes only the parser-only categories (A/B/D + parser-miss C) and excludes F/G. **Do not** use PascalCase variant names like `VerbVariation` — those are never emitted to JSON.
- `--rawfile`, not `--slurpfile`. Reads the file as a single string; `split("\n") | map(select(length > 0))` produces a clean `[string]` array. `--slurpfile` would require NDJSON semantics and force a file-format change.
- `awk '!seen[$0]++'` after jq preserves the `sort_by(-.cards_unlocked)` priority ranking while deduping. jq's `unique` would re-sort alphabetically and lose priority.
- Single jq invocation via terminal `| .[]` to stream strings — no double jq pipe.

**Category inclusion.** Include Category C (TriggerEffect) alongside A/B/D. C fires when a trigger mode parses but a co-occurring `Effect:*` gap exists; that effect gap is often a parser miss, not runtime work. Route the human to the `Effect:` gap's `source_text` (step 2 below). Genuinely-runtime C cards fall out at grep time (step 3) — skip them there, don't force runtime work into this loop. Skip F (NewMechanic) entirely; those belong in `unlock-set`.

Empty `/tmp/batch.txt` → the quick-win pool for this format is exhausted for this session. Stop or switch formats.

### Phase 1.5 — Alternative selector: swallow-warning batching

When the parser-gap-analyzer pool is thin or you want to target a specific *anti-pattern class* (rather than verb-variation cards), batch by `parse_warnings` instead. The swallow detectors in `crates/engine/src/parser/swallow_check.rs` flag cards where the AST silently dropped Oracle text — `Swallow:Condition_If`, `Swallow:DynamicQty`, `Swallow:Duration_ThisTurn`, `Swallow:Optional_YouMay`, `Swallow:Condition_Unless`, `Swallow:Replacement_Instead`, etc. Each prefix is a recognition-without-binding bug class — fix the dispatch site once and the whole class flips.

```bash
# Batch 10 cards from a specific swallow class (uses fresh card-data.json — Phase 0 already regenerated it)
CLASS='Swallow:DynamicQty'   # or Swallow:Condition_If, Swallow:Duration_ThisTurn, etc.
jq -r --arg ex "$(cat /tmp/velocity-flipped.txt | tr '\n' '|')" --arg cls "$CLASS" '
  to_entries[]
  | select(.value.parse_warnings // [] | any(startswith($cls)))
  | .key' client/public/card-data.json \
  | grep -vxE "${ex%|}" | head -10 > /tmp/batch.txt
```

Use this selector when:
- A specific swallow prefix dominates the warning histogram (e.g., 750 `DynamicQty` cards) — fixing the dispatch site cascades across hundreds of cards.
- You want to drive a metric down deliberately (e.g., "eliminate `Replacement_Instead` swallows this session").
- The parser-gap-analyzer near-miss list is exhausted for the format.

The peek-vs-chomp anti-pattern (see `/oracle-parser` §10) is the recurring root cause — an upstream `scan_*` reads the marker without consuming, downstream loop re-encounters and warns. Diagnose by tracing one card end-to-end before editing.

---

## Phase 2 — Batched inner loop (5–10 edits per compile cycle)

**Do NOT compile between cards.** Make all batch edits first, then compile once.

For each batch:

1. **Read each card's gap.** Query `client/public/coverage-data.json` for each card's `gap_details[].source_text` and `parse_details` tree. The `source_text` is the exact Oracle snippet that failed.
   ```bash
   for card in $(cat /tmp/batch.txt); do
     echo "=== $card ==="
     jq --arg n "$card" '.cards[] | select(.card_name == $n) | .gap_details' \
       client/public/coverage-data.json
   done
   ```
2. **Find the analogous existing combinator.** Grep `crates/engine/src/parser/oracle_nom/` and the relevant `oracle_*.rs` for a similar phrase already handled. Almost always a `tag()` arm added to an existing `alt()` (per CLAUDE.md's "Compose nom combinators, don't enumerate permutations").
3. **For Category C cards:** if no analogous parser combinator exists and the gap requires runtime work (new resolver handler, new event matcher, new CR-grounded behavior), **skip the card**. Don't force runtime work into the velocity loop — that's `unlock-set` territory.
4. **Edit the whole batch.** One parser file per card (or shared file for related cards). No compile between edits.
5. **Compile + parser test.**
   ```bash
   cargo test -p engine --lib parser::
   ```
   One compile (~60s cold, ~10–30s warm), then 2150 parser tests at 0.26s. Passing = high confidence you haven't broken existing patterns. For a faster mid-batch "did I break types?" signal, use `cargo check -p engine` (no test-binary codegen).
6. **Validate the batch flipped.**
   ```bash
   cargo run --profile test --bin oracle-gen --features cli -- data \
     --filter "$(paste -sd'|' /tmp/batch.txt)" \
     > /tmp/batch-ast.json 2>/dev/null
   ```
   Live MTGJSON parse exercises your edited code. If `--profile test` + `--features cli` is incompatible on your toolchain (verify once per machine at skill-authoring time), fall back to `--profile tool` — but expect a second full rebuild per batch because of the test→tool profile flip.
7. **Append flipped cards to the exclude set — runs even if step 6 errors.** A card is flipped if its AST no longer contains any `Unimplemented` / `Unknown(…)` / `Unrecognized` tokens, recursively. The jq below walks the entire card JSON (via `.. | objects`) so nested/modal/chained effects are caught:
   ```bash
   jq -r 'to_entries[] | select(
     # No `type: "Unimplemented"` anywhere in the AST (effects, costs, nested modals, etc.)
     ([.value | .. | objects | select(.type? == "Unimplemented")] | length == 0)
     # No externally-tagged `{"Unknown": "..."}` anywhere — TriggerMode::Unknown serializes this way.
     # `tostring | startswith("Unknown")` does NOT work: tostring on the object returns the JSON
     # string "{\"Unknown\":\"...\"}" which starts with "{", not "Unknown".
     and ([.value | .. | objects | select(has("Unknown"))] | length == 0)
     # No `type: "Unrecognized"` anywhere (static conditions, nested conditions, etc.)
     and ([.value | .. | objects | select(.type? == "Unrecognized")] | length == 0)
   ) | .key' /tmp/batch-ast.json >> /tmp/velocity-flipped.txt
   ```
   **Caveat for Category D (StaticCondition).** `Static:Unrecognized` gaps may surface only in the coverage classifier's `gap_details`, not necessarily as an `Unrecognized` node in the card's AST. If a D-category card doesn't flip via the walk above, re-check in the next Phase 1 batch — if it no longer appears, append manually to the exclude file. If step 6 errored entirely (oracle-gen crash, profile incompat), manually append any cards you know flipped and proceed.
8. **Loop to Phase 1.** Continuous. Interrupt at any time — the exclude set preserves progress.

---

## Phase 3 — Gate (once per session, or on demand)

Run when wrapping up or at a natural stopping point (queue empty, pattern family exhausted, end of work session):

```bash
cargo fmt --all
cargo clippy -p engine --all-targets -- -D warnings  # engine only — parser changes don't touch downstream crates
cargo test -p engine                                  # engine suite — parser + game + types
./scripts/gen-card-data.sh                            # regen for coverage/semantic-audit determinism
cargo coverage                                        # final flip count (reads fresh card-data.json)
cargo semantic-audit                                  # catch over-matching
./scripts/snapshot-regression.sh /tmp/card-data-before.json
rm -f /tmp/velocity-flipped.txt                       # clear session state
```

**Do NOT use `cargo test-all` in this gate.** Parser-only changes don't touch
`engine-wasm`, `phase-ai`, or `server-core` code paths — `cargo test -p engine`
is authoritative. `cargo test-all` adds ~5 minutes of unrelated test runtime.
The only time to escalate to `test-all` is if you knowingly changed an AST type
signature exported across crate boundaries (e.g., added a variant to
`TriggerMode`). That is rare and belongs in `unlock-set`, not velocity.

**`gen-card-data.sh` runs at the top of Phase 3** so coverage + semantic-audit see the freshest state rather than the Phase 0 snapshot.

On gate failure: diagnose, fix, re-gate. Do not partially commit a batch that introduced downstream breakage.

On gate success: one velocity commit, or a small set grouped by pattern family (not per card). Commit message lists the pattern families extended and the flip count.

---

## Phase 4 — Resuming across sessions

Re-invoke the skill. Phase 0's `: > /tmp/velocity-flipped.txt` clears any stale exclude-set file from a crashed prior session, and `gen-card-data.sh` refreshes `card-data.json` (no harm if the last gate already did this). No cross-session memory needed — the analyzer is authoritative.

---

## Explicit non-goals

- **Not a replacement for `unlock-set`.** Cluster-level infrastructure (new typed primitives, CR-grounded mechanics, runtime work) still goes through `unlock-set`.
- **Not skipping gates.** All the same gates still run — just once per session.
- **Not an agent workflow.** No `engine-implementer` spawning — that re-introduces the heavyweight per-cluster review overhead this skill exists to escape.
- **Not a persisted queue.** `/tmp/velocity-flipped.txt` is session-scoped, resets at gate, has no meaning across sessions.
- **Not time-boxed.** Sprints run as long as you want.

---

## Recurring pitfalls

| Pitfall | Symptom | Fix |
|---|---|---|
| Compiling per card | Each edit takes 60s | Batch 5–10 edits, compile once. |
| Including Category F | Forced into runtime work | The jq filter explicitly excludes `NewMechanic` — keep it that way. |
| Treating C as all-runtime | Missed parser quick wins | Include C, skip at step 3 if truly runtime. |
| Skipping `gen-card-data.sh` at Phase 3 | Coverage report shows stale numbers | Always regen at the top of the gate. |
| Per-batch `gen-card-data.sh` | Destroys velocity | Never; exclude set replaces that purpose. |
| Using `--slurpfile` in Phase 1 | jq errors or returns empty | Use `--rawfile` + `split("\n")`. |
| Writing new string-matching parser code | CLAUDE.md violation | Always nom combinators; see `/oracle-parser` skill. |
