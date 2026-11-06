---
name: validate-cr-annotations
description: Use when writing, modifying, or reviewing CR (Comprehensive Rules) annotations in engine code — any time a `// CR` or `/// CR` comment is added, changed, or audited. Ensures every CR number is verified against `docs/MagicCompRules.txt` before it enters the codebase.
---

# CR Annotation Validation

This skill enforces the mandatory verification protocol for MTG Comprehensive Rules annotations. **Every CR number is verified by grep before it is written.** No exceptions.

---

## Verification Protocol

**For every CR number you are about to write, run this BEFORE writing:**

```bash
grep -n "^NNN.X" docs/MagicCompRules.txt
```

Where `NNN.X` is the rule number (e.g., `702.33d`, `613.1f`, `120.2a`).

- If the grep returns a matching line → the rule exists. Read the text to confirm it matches your intent.
- If the grep returns nothing → **STOP. Do not write the annotation.** The rule number is wrong.

### Batch verification for multiple annotations

When adding several CR annotations in one change, verify all at once:

```bash
for ref in "702.33d" "613.1f" "120.2a"; do
  echo "=== CR $ref ==="
  grep -n "^${ref}" docs/MagicCompRules.txt | head -2
done
```

---

## Format Rules

| Format | When to use | Example |
|--------|------------|---------|
| Inline comment | Implementation code | `// CR 704.5a: A player with 0 or less life loses the game.` |
| Doc comment | Public items (functions, types, variants) | `/// CR 704: Checks state-based actions.` |
| Combined `+` | Multiple interacting rules | `// CR 702.2c + CR 702.19b: Deathtouch with trample assigns lethal (1) to each blocker.` |
| Combined `/` | Alternative/overlapping rules | `// CR 704.3 / CR 800.4: SBAs may have ended the game during phase auto-advance.` |

**Mandatory rules:**
- Prefix is always `CR` — never `Rule`, `MTG Rule`, or bare numbers
- Number format regex: `CR \d{3}(\.\d+[a-z]?)?`
- **Description is mandatory** — a bare `CR 704.5a` with no explanation is not acceptable
- Placement: directly above or inline with the code that implements the rule

---

## Known Hallucination Patterns

These are the error classes found in the full audit of this codebase. Watch for them:

### 1. Off-by-one section numbers (most common)

The 702.x keyword ability numbers are arbitrary sequential assignments. There is no mnemonic anchor. Common confusions:

| Keyword | WRONG | CORRECT |
|---------|-------|---------|
| Kicker | CR 702.32 (= Fading) | **CR 702.33** |
| Flash | CR 702.8c/d (don't exist) | **CR 702.8a/b** (only two sub-rules) |
| Exploit | CR 702.110c (doesn't exist) | **CR 702.110a/b** (only two sub-rules) |
| Shroud | CR 114.x (= Emblems!) | **CR 702.18a** |

**Rule: Never assume you know a 702.x number. Always grep.**

### 2. Hallucinated lettered sub-rules

A parent rule exists but the lettered sub-rule does not:

| WRONG | WHY | CORRECT |
|-------|-----|---------|
| CR 613.3d | 613.3 has no sub-rules | **CR 613.1d** (Layer 4 types) |
| CR 613.3f | 613.3 has no sub-rules | **CR 613.1f** (Layer 6 abilities) |
| CR 716.5/716.6 | 716 ends at 716.4 | **CR 716.2a** (class level) |
| CR 719.4 | 719 ends at 719.3c | **CR 719.3c** (Case solved) |
| CR 118.12d | 118.12 has a/b only | **CR 118.5** (zero costs) |
| CR 120.1b | doesn't exist | **CR 120.2b** (noncombat damage) |

**Rule: After grepping, also check that the sub-rule letter exists — `grep -n "^613.1[a-g]" docs/MagicCompRules.txt` to see all sub-rules.**

### 3. Wrong section entirely

| WRONG | What it actually is | Intent | CORRECT |
|-------|-------------------|--------|---------|
| CR 114.x | Emblems | Targeting | **CR 115.x** |
| CR 711.x | Leveler cards | DFC transform | **CR 712.x** |
| CR 702.32 | Fading | Kicker | **CR 702.33** |

**Rule: After finding the rule text, read the first sentence to confirm the section topic matches your intent.**

---

## Frequently Referenced Sections

Quick-reference for sections that are commonly annotated in this codebase:

| Section | Topic | Key sub-rules |
|---------|-------|---------------|
| **120** | Damage | 120.1 (what can be damaged), 120.2a (combat), 120.2b (noncombat), 120.3a-h (results), 120.4 (sequence) |
| **601** | Casting spells | 601.2a-h (steps), 601.2f (hybrid/Phyrexian) |
| **603** | Triggered abilities | 603.2 (when triggered), 603.3 (APNAP stack order), 603.4 (intervening-if) |
| **608** | Resolving spells/abilities | 608.2b (partial fizzle), 608.2e (modes) |
| **613** | Continuous effects / Layers | 613.1a-g (layers 1-7), 613.2a-c (layer 1 sublayers), 613.4a-d (layer 7 sublayers) |
| **614** | Replacement effects | 614.1a (definition — "instead"), 614.4 (must pre-exist), 614.15 (self-replacement effects) |
| **616** | Multiple replacements | 616.1a (self-replacement priority), 616.1b-d (ordering), 616.1e (player choice) |
| **700** | General | 700.4 (damage memory) |
| **701** | Keyword actions | Sequential numbers — always verify |
| **702** | Keyword abilities | Sequential numbers — always verify |
| **704** | State-based actions | 704.5a-y (individual SBAs) |
| **711** | Leveler cards | Not DFCs! |
| **712** | Double-faced cards | 712.2 (nonmodal/transform), 712.3 (modal) |
| **716** | Class cards | 716.2a (level ability), 716.2b-d (level designation) |
| **719** | Case cards | 719.3a (to solve), 719.3b (solved designation), 719.3c (solved ability) |

---

## Self-Check Before Finalizing

After writing CR annotations, ask yourself:

1. **Did I grep every CR number?** — No number enters code unverified.
2. **Does the rule text match my description?** — Read the actual CR text, not just confirm the number exists.
3. **Is the sub-rule letter real?** — Parent existing doesn't mean child exists.
4. **Is this the right section?** — CR 114 ≠ CR 115; CR 711 ≠ CR 712.
5. **Did I include a description?** — Bare `CR NNN.Xa` without explanation is not acceptable.
6. **Is there an existing annotation nearby I should update?** — Old formats (`Rule 514.1`, `MTG Rule 727`) get migrated to `CR` format.
