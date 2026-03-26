---
name: audit-card-parsing
description: Semantic audit of card-data.json — compare parsed JSON structures against Oracle text to find misparses in cards the coverage system marks as "supported." Supports --after for parallel agent runs and appends to a shared report.
---

# Audit Card Parsing Accuracy

Semantically compare the parsed JSON ability structures in `client/public/card-data.json` against the raw Oracle text for each card. Find cards that the coverage system considers "supported" (no `Unimplemented` effects, no `Unknown` triggers/keywords) but whose parsed structures are **semantically incorrect** — wrong effect type, wrong target, wrong amount, missing abilities, missing conditions, etc.

## Subcommands

- **`continue`** — Resume from where the last audit left off. Reads the "Resume Point" section from the report file to get the `--after` value automatically. Equivalent to `/audit-card-parsing --after "<last card>"`.
- **`status`** — Print a summary of the audit: how many cards audited, how many remaining, number of open vs fixed patterns, and the resume point. Does NOT run any audit — just reads the report file and prints stats.

## Parameters

The user may append parameters after invoking this skill:

- **`--after "Card Name"`** — Start processing cards alphabetically after this card name (case-insensitive key). Use this to split work across parallel agents.
- **`--limit N`** — Process N cards per batch (default: 50).
- **`--format <format>`** — Only audit cards legal in this format (e.g., `standard`, `modern`, `pioneer`, `commander`).
- **`--file <path>`** — Append findings to this report file (default: `.planning/parser-audit-report.md`).

If no parameters or subcommand given, start from the beginning and process 50 cards.

When `continue` is used, read the report file's "Resume Point" section to extract the last card key, then proceed as if `--after "<that key>"` was passed.

## Instructions

### Step 1 — Extract the batch

Use `jq` to extract a batch of supported cards from `client/public/card-data.json`. "Supported" means:
- Zero `Unimplemented` effects in the entire ability tree (including sub_ability, else_ability, mode_abilities)
- Zero `Unknown` trigger modes
- Zero `Unknown` keywords
- At least one parsed item (abilities + triggers + static_abilities + replacements > 0)
- Oracle text longer than 20 characters (skip vanilla creatures)

Apply `--after` and `--format` filters if specified. Sort by key (alphabetical). Take `--limit` cards.

Use this jq template (adjust filters as needed):

```bash
jq -r '[to_entries[]
  | select(.key > "<AFTER_KEY_LOWERCASE>")
  | select(
      (.value.oracle_text | length) > 20 and
      ([.value.abilities[]? | select(.effect.type == "Unimplemented")] | length) == 0 and
      ([.value.triggers[]? | select(.mode | type == "object" and has("Unknown"))] | length) == 0 and
      ([.value.keywords[]? | select(type == "object" and has("Unknown"))] | length) == 0 and
      ((.value.abilities | length) + (.value.triggers | length) + (.value.static_abilities | length) + (.value.replacements | length)) > 0
    )
  | {key, name: .value.name, oracle_text: .value.oracle_text, abilities: .value.abilities, triggers: .value.triggers, statics: .value.static_abilities, replacements: .value.replacements, keywords: .value.keywords}
] | sort_by(.key) | .[:50]' client/public/card-data.json
```

For `--format` filtering, add: `and (.value.legalities.<format> == "legal")`

### Step 2 — Semantic comparison

For each card in the batch, compare the Oracle text against the parsed JSON. Check every one of these dimensions:

#### A. Effect type correctness
Does the parsed effect type match the Oracle verb?
- "deals N damage" → `DealDamage` (not `LoseLife`)
- "gains lifelink/flying/etc." → keyword grant via `Pump` or static (not `GainLife`)
- "gains N life" → `GainLife`
- "destroy" → `Destroy` (not `ChangeZone` to graveyard)
- "exile" → `ChangeZone` with destination `Exile`
- "return to hand" / "return to its owner's hand" → `Bounce`
- "counter target spell" → `Counter`
- "search your library" → `SearchLibrary`
- "draw N cards" → `Draw`
- "create a token" → `Token`
- "put a +1/+1 counter" → `PutCounter`
- "sacrifice" → `Sacrifice`
- "discard" → `Discard`
- "tap/untap" → `Tap`/`Untap`
- "scry N" → `Scry`
- "surveil N" → `Surveil`
- "mill N" → `Mill`
- "fights" → `Fight`

#### B. Target correctness
- "target creature" → filter should include `Creature` type
- "target player" / "target opponent" → `Player` / `Opponent` filter
- "any target" → `Any` target type
- "each creature" / "all creatures" → should NOT have a single-target filter; should be `DamageAll`, `DestroyAll`, or untargeted
- "target creature or planeswalker" → filter should include both types
- "another creature you control" → `Another` property + `You` controller

#### C. Amount/quantity correctness
- "deals 3 damage" → amount should be `Fixed(3)`
- "equal to its power" → should reference `TargetPower` or `EventContextSourcePower`
- "X damage where X is..." → should be a dynamic `Ref` quantity, not `Fixed`

#### D. Missing abilities
- Count distinct ability lines in Oracle text (split by `\n`). Each non-keyword, non-reminder-text line should produce at least one parsed item (ability, trigger, static, or replacement).
- "When/Whenever/At the beginning of" → should produce a trigger
- "gets +N/+M" / "has <keyword>" as a permanent state → should produce a static ability
- "If [this] would [X], [Y] instead" → should produce a replacement

#### E. Condition/constraint accuracy
- "you may" → ability should have `optional: true`
- "if you do" → should chain via sub_ability with a condition or pay-gate
- "as long as" → should be a static with a condition (not an unconditional static)
- "for each" / "equal to the number of" → should use a dynamic quantity, not fixed
- "only as a sorcery" → `sorcery_speed: true`

#### F. Sub-ability chain completeness
- Multi-sentence effects like "Exile target creature. Its controller gains life equal to its power." → first effect should have a `sub_ability` for the second part
- "then" clauses → should chain as sub_ability
- "If you do, [effect]" after an optional cost → sub_ability gated on the cost

#### G. Keyword correctness
- Keywords in Oracle text (flying, trample, haste, etc.) should appear in the `keywords` array
- Parameterized keywords ("ward {2}", "crew 3") should have the correct parameter value

#### H. Trigger configuration
- "When [this] enters" → `ChangesZone` with destination `Battlefield`
- "When [this] dies" → `ChangesZone` with origin `Battlefield`, destination `Graveyard`
- "Whenever [this] attacks" → `Attacks` mode
- "At the beginning of your upkeep" → `Phase` mode with phase `Upkeep` and your-turn constraint
- Trigger `valid_card` should match what the Oracle text describes (SelfRef for "this creature", typed filter for "a creature")

### Step 3 — Read the skip list

Before auditing, read the report file header. It contains a **Skip List — Known Unfixed Patterns** section listing patterns that have already been documented but not yet fixed in the parser. These will still appear in the card data, but agents should NOT create new entries for them — only add new example cards under the existing pattern if found.

Fixed patterns do not need a skip list — once the parser is fixed and card data is regenerated, those patterns simply won't appear in the JSON anymore.

Only report **genuinely new patterns** not already in the skip list.

### Step 4 — Group new findings by parser pattern

As you audit cards, group issues by the **underlying parser gap** — the root cause in the parser that produces the mismatch. Many cards will hit the same parser gap. Do NOT record the same pattern multiple times; instead, collect example cards under the pattern.

A "pattern" is a class of Oracle text that the parser handles incorrectly in the same way. For example:
- "X and Y" conjunction in a single clause → only X is parsed, Y is dropped (affects Blood Artist, Cauldron Familiar, Guide of Souls, etc.)
- "you don't control" controller filter → controller field is null instead of Opponent (affects Celestial Regulator, Devoted Grafkeeper, etc.)

**If a card has a truly unique one-off issue** (not a systemic pattern), record it as its own pattern with one example.

#### Pattern format

Each pattern should include:
- **Pattern name** — short descriptive name for the parser gap
- **Category** (one of: `wrong-effect-type`, `wrong-target`, `wrong-amount`, `missing-ability`, `missing-condition`, `missing-sub-ability`, `wrong-keyword`, `wrong-trigger-config`, `wrong-static`, `wrong-replacement`)
- **Severity**: `high` (will cause wrong game behavior), `medium` (partially wrong but may work in common cases), `low` (minor inaccuracy unlikely to matter)
- **Parser gap description** — what the parser does wrong and what it should do instead
- **Oracle pattern** — the Oracle text structure that triggers this gap (e.g., `"[effect1] and [effect2]"` in a single clause)
- **Example cards** — 2-5 cards that hit this pattern, each with a one-line description showing the mismatch
- **Estimated scope** — rough guess at how many cards this pattern likely affects beyond what you've seen (e.g., "likely 50+ cards" or "probably just these 2")

### Step 5 — Write to report

Append findings to the report file (default `.planning/parser-audit-report.md`). If the file doesn't exist, create it with the header. If it exists, read it first, then:
- **Merge with existing patterns.** If the file already contains a pattern that matches one you found, add your new example cards under that pattern instead of creating a duplicate entry. Update the estimated scope if your new examples change the picture.
- **Add new patterns** that don't already exist in the report.
- **Always add a batch log entry** at the bottom recording what range you audited.

#### Report format

```markdown
# Parser Accuracy Audit Report

Generated by `/audit-card-parsing`. Organized by parser gap pattern, not by individual card.

## Skip List — Known Unfixed Patterns

Already documented. Agents should NOT re-report these — only add new example cards if found.

1. [pattern description] ([est. scope])
...

---

## Pattern: [short descriptive name]
**Category:** [category] | **Severity:** [high/medium/low] | **Est. scope:** [N+ cards]

**Parser gap:** [What the parser does wrong. What it should do instead.]

**Oracle pattern:** `[the Oracle text structure that triggers this]`

**Examples:**
- **[Card Name]**: "[oracle excerpt]" → parsed as [X], should be [Y]
- **[Card Name]**: "[oracle excerpt]" → parsed as [X], should be [Y]

---

## Pattern: [next pattern...]

...

---

# Batch Log

| Range | Cards Audited | Patterns Found | New Patterns | Date |
|-------|--------------|----------------|--------------|------|
| "first key" → "last key" | N | M total hits | K new | YYYY-MM-DD |
```

### Step 6 — Update resume point and report progress

After writing findings:

1. **Update the "Resume Point" section** at the top of the report file with the last card key processed. This is critical for `continue` to work.
2. Output to the user:
   - The range of cards audited (first key → last key)
   - How many cards were audited, how many had issues, how many unique patterns
   - The last card key processed
   - List of pattern names found (marking which are new vs already in report)

Example:
```
Audited: "aether vial" → "angel of mercy" (50 cards, 12 cards with issues, 5 unique patterns)
Last key: "angel of mercy"
Next run: /audit-card-parsing --after "angel of mercy"
Patterns:
  [NEW] "X and Y" conjunction not split into sub_ability chain (4 examples, est. 50+ cards)
  [NEW] "you don't control" controller filter missing (3 examples, est. 30+ cards)
  [NEW] "for each" dynamic quantity parsed as Fixed (2 examples, est. 20+ cards)
  [EXISTING] keyword grant parsed as GainLife (1 new example added)
  [NEW] multi-modification static only captures first modification (2 examples, est. 15+ cards)
```

## Severity guidelines

- **high**: The parsed effect will produce observably wrong game behavior. Examples: wrong effect type entirely (GainLife instead of keyword grant), missing an entire ability line, wrong target type (Any instead of Creature).
- **medium**: The parse captures the gist but gets a detail wrong. Examples: missing `optional: true` on a "you may" ability, static condition not captured but the keyword grant is correct, wrong quantity type but happens to produce the right number in common cases.
- **low**: Minor inaccuracy. Examples: `sorcery_speed` not set on an activated ability (if there are other safeguards), cosmetic description mismatch, trigger constraint slightly off.

## Important notes

- **Do NOT flag Un-set / acorn cards** (silver-bordered / Unfinity acorn stamps). These often have unparseable mechanics by design ("wearing a hat", "roll to visit Attractions"). Skip them.
- **Do NOT flag cards with only `non_ability_text`** issues — that field is for flavor/reminder text the parser intentionally ignores.
- **Be conservative.** Only flag issues where you are confident the parse is wrong. If you're unsure whether a parse is correct, skip it. False positives waste more time than missed issues.
- **GenericEffect is a wildcard.** The `GenericEffect` type is used for complex effects that don't fit a specific category. Don't flag a GenericEffect as wrong-effect-type unless you're certain a specific effect type should have been used instead.
- **Check for TargetOnly effects.** `TargetOnly` is a placeholder used when the parser extracts a target but can't determine the effect. This IS a parsing gap but is already tracked by the coverage system, so don't flag it.
- **Energy costs** (`PayEnergy`) are a cost type, not an effect — don't flag "pay {E}" as a missing ability.
- **Ability costs** (tap, mana, sacrifice, life, energy) live in the `cost` field of the ability, not as separate effects. Don't flag them as missing abilities.
