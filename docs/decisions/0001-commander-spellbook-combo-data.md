# 0001 — Use the Commander Spellbook bulk combo export

- **Status:** accepted
- **Decided by:** Erckdd (repository owner)
- **Date:** 2026-09-27
- **Answers:** the Gate in plan 61-08 (P7), "May phase.rs generate, host and ship a
  derived artifact from the Commander Spellbook bulk combo export?"

## Decision

**Yes.** phase.rs may generate, host and ship a derived, facts-only combo table built from
the Commander Spellbook bulk export, under the risk-reduction measures the plan applies
unconditionally to a YES:

1. Facts-only projection: card names, the pair relation, mana values, a curated-status code
   and a play count. Contributor-authored prose (`result` strings, `notablePrerequisites`) is
   never carried.
2. The emitted schema has no free-text field; every carried card name must resolve in the
   card database at generation time or the row is dropped.
3. The artifact is gitignored and served from R2, exactly like `card-data.json`, so nothing is
   bundled in the repository (the commitment `DMCA.md` makes).
4. Attribution and the snapshot date are rendered on every surface that shows a match.

## Basis stated by the owner

Commander Spellbook is MIT licensed.

## Scope of that licence, as verified when the plan was written (2026-09-12)

The MIT licence covers the Commander Spellbook website and backend source code. The bulk
combo export itself carries no stated licence, attribution requirement or terms of use, and
Spellbook is Fan Content on Wizards of the Coast IP. The owner accepts this provenance with
the four measures above in place. A takedown request would be answered by removing the
hosted artifact and the feature that reads it.
