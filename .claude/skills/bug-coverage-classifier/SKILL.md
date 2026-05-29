---
name: bug-coverage-classifier
description: Use when classifying a Phase bug report against a specific card's engine-authoritative parse_details. The primary job is validating that a card CLAIMS to support the misbehaving clause and then comparing the parsed AST against the Oracle text to confirm or refute that claim. Unsupported clauses are already known gaps and get deferred. Reads the same coverage data the Alt-hover overlay and card-bot render. Invoked by /bug-triage and /issue-clusterer.
---

# Bug Coverage Classifier

## What this skill is for

The actionable signal is **`supported_aspect_defect`** — a card claims to support the clause the user is reporting on, but the engine is still misbehaving. That is the bug worth filing.

Everything else is either already-known or out-of-scope:

- `unsupported_aspect` — explicitly marked `supported: false` in `parse_details`. We already know it doesn't work. Defer unless the fix is trivial. No investigation needed beyond confirming the verdict.
- `not_card_data_attributable` — the bug is not about card text at all (combat assignment, AI behavior, UI, multiplayer, deckbuilder). Route to the relevant subsystem; the parser/effect-handler can't be the cause.
- `cannot_determine` — the report doesn't pin to any clause. Ask for clarification.

Spend your effort on `supported_aspect_defect`. That is where bugs actually live.

## Inputs

- `card_name` — the card the bug report names (e.g. `"Archon of Cruelty"`).
- `bug_description` — what the user said is misbehaving.

## What to do

### 1. Look up the card

```bash
curl -s "https://pub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev/preview/coverage-data.json" \
  | jq --arg n "<card_name>" '.cards[] | select(.card_name == $n) | {oracle_text, supported, parse_details}'
```

For repeat lookups in the same session, cache the full coverage JSON to a tmp file once and `jq` against the file.

If `jq` returns nothing, try common name normalizations: drop apostrophes (`Bloodchief's Ascension` → `Bloodchief Ascension`), try the front face of an MDFC, fix obvious typos. Consult `triage/unknown-card-mapping.json` for known corrections. If the name is flagged `not_a_card` (e.g. token types like "Blood Token"), return verdict `not_card_data_attributable`.

### 2. Identify which clause the bug is about

Read the bug description and match it to the parse_details node whose `source_text` describes the misbehaving line. The card name is already pinned, so you're choosing among at most a handful of clauses. Use natural-language judgement — bug reports use synonyms, abbreviations, and game-state examples (`"X=3"`, `"on end step"`, `"with 2 counters"`) that won't always share vocabulary with the Oracle text.

If the bug isn't about card text at all (UI, AI, combat, MP), return `not_card_data_attributable`.

If you genuinely cannot decide which clause, return `cannot_determine`.

### 3. Quick short-circuit: is the matched clause unsupported?

If the matched node has `supported: false`, the verdict is **`unsupported_aspect`**. Return it and stop — this is already-known gap territory, the user is correct that the card misbehaves but there's no engine work to investigate beyond what's already on the backlog.

### 4. The real work: AST faithfulness check on supported clauses

If the matched node has `supported: true`, do NOT take that at face value. The `supported` flag means "the parser produced a typed node for this clause" — it does NOT prove the typed node is faithful to the Oracle text. The two most common defect shapes are:

**A. Missing children / dropped sub-effects.** A complex clause like Archon of Cruelty's *"target opponent sacrifices a creature or planeswalker of their choice, discards a card, and loses 3 life. You draw a card and gain 3 life"* should produce a chain of effect children covering all five sub-effects (Sacrifice, Discard, LoseLife, Draw, GainLife). If `children` is missing any of those, the parser collapsed the chain. The user's report ("discard not resolving") will match exactly the missing child.

**B. Collapsed/incorrect details.** Bloodroot Apothecary's *"you and target opponent each create a Treasure token"* should encode two beneficiaries. If the token child only records one beneficiary, or the token shape itself is wrong (`+0/+0 Treasure (Artifact Treasure)` looks like a stat-line token rather than a real Treasure with its sacrifice ability), the parser collapsed structure. Likewise, the `valid target`, `watches`, `optional`, `count`, `amount`, and other `details` keys should match what the Oracle text says — a missing "an opponent controls" qualifier or a wrong counter quantity is the same class of defect.

For each supported clause the bug touches, read the Oracle source_text alongside the parsed `children` and `details` and ask:

- Does the children chain cover every effect in the Oracle source_text?
- Do the details fields match the qualifiers, quantities, and targets in the Oracle source_text?
- Does the labeled effect type match what the Oracle text describes (e.g. is `BecomesTarget` filtered to opponent-controlled spells, or is the filter absent)?

If any answer is no, the verdict is **`supported_aspect_defect`** and your `reasoning` should cite the specific AST gap so the maintainer knows where to start. The skill's value lives in pointing at the dropped child, the missing detail, the wrong qualifier — not just emitting a verdict label.

If the AST looks faithful and you still believe the bug is real, the verdict is still `supported_aspect_defect` (likely a runtime bug rather than a parser misparse), but say so explicitly in the reasoning.

## Output shape

```json
{
  "card_name": "<as input>",
  "verdict": "supported_aspect_defect | unsupported_aspect | not_card_data_attributable | cannot_determine",
  "matched_clause": {
    "label": "<engine label>",
    "source_text": "<Oracle fragment>",
    "supported": true | false
  } | null,
  "reasoning": "<for supported_aspect_defect: cite the specific AST gap — which child is missing, which detail is wrong, which qualifier is absent. For unsupported_aspect: one line confirming the supported:false node. For the other verdicts: one line.>"
}
```

## Posting back to a GitHub issue

When the classifier output corresponds to an existing GH issue (the publish path filed it with `classifier:pending`), the operator/skill caller also:

1. Posts the reasoning as a comment with body:
   ```
   ## Classifier analysis

   **Verdict**: `<verdict>`

   **Matched clause**: `<source_text or "(none)">`  (supported: <true|false>)

   **Reasoning**: <as above>

   ---
   _Posted by bug-coverage-classifier skill._
   ```
2. Swaps the `classifier:pending` label for one of:
   - `classifier:supported-aspect-defect` (the actionable bucket — red)
   - `classifier:unsupported-aspect` (known gap — grey)
   - `classifier:not-card-data` (wrong subsystem — blue)
   - `classifier:cannot-determine` (needs reporter follow-up — yellow)

   ```bash
   gh issue edit <num> --remove-label classifier:pending --add-label classifier:<verdict-slug>
   ```

The backlog of unannotated issues is greppable as `gh issue list -l classifier:pending`.

## Why this matters

The backlog is already overwhelmingly known gaps. Filing more `unsupported_aspect` reports just adds noise on top of work the maintainer already has tracked. The reports that move the project forward are the ones where the engine is *lying* about supporting a clause — those go on the floor unnoticed unless someone reads the AST against the Oracle text. That comparison is exactly this skill's job.

## When you are the caller

If you're invoking this skill from `/bug-triage` or `/issue-clusterer`, pass `(card_name, bug_description)` per report. Use the verdict to inform the NEW / DUP / APPEND / HANDLED decision — not to gate it. Maintainer review is still the final arbiter, and the reasoning field is what gets pasted into the GH issue body so the next engineer has a concrete starting point.
