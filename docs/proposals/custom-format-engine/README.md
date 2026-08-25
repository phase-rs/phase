# Custom ("Design Your Own") Format Engine — Design Proposal

**Status:** design merged. Implementation is underway, phase by phase — see
`IMPLEMENTATION_PLAN.md` for the sequencing and current status of each
phase.

This is a design proposal for a general, data-driven custom-format layer,
motivated by wanting to support four Eternal Central retro formats (Old
School 93-94, Old School 95, Middle School, Classic Magic) without hardcoding
four new `GameFormat` enum variants.

Originating discussion: [phase-rs/phase#5312](https://github.com/phase-rs/phase/discussions/5312).

- **CONTEXT.md** — why this matters, what's already confirmed against the
  current codebase, open questions, how a maintainer's informal framing
  ("FFA that's super flexible and you can save a configuration as a custom
  format") resolved the delivery-surface question, and why Swedish Old School
  93/94 (a distinct, real ruleset — different restricted list, no mana burn)
  became the phase-1 preset instead of the four EC formats.
- **RESEARCH.md** — the detailed investigation: current `GameFormat`/
  `FormatConfig` architecture, the four EC formats' verbatim rules, legacy
  rules deltas (mana burn, damage-on-the-stack, pre-M10 Wish, the legend
  rule's historical scope change), and cross-checks against other in-flight
  format work.
- **PLAN.md** — the proposed schema (`CustomFormatRules` split into a
  structural `StructuralRules` axis and a legality `LegalityRules` axis), how
  formats parameterize as data, and the two-phase sequencing (§8): phase 1 is
  the schema + an existing-lobby "save as custom format" action + Swedish Old
  School (needs no legacy-rules engine work at all); phase 2 is the four EC
  formats plus the legacy-rules wiring (mana burn, damage-on-the-stack, wish,
  legend rule) they need.
- **IMPLEMENTATION_PLAN.md** — the phased build-out charter derived from
  this design: seven phases (1a–2cd), what each delivers, their
  dependencies, and any correction a phase's own plan review surfaced along
  the way. `CombatDamageTiming::OnStack` (and the two EC presets that need
  it) is deliberately excluded — a separate, later, larger sub-project per
  §6/§7/§8 below.

The design (`CONTEXT.md`/`RESEARCH.md`/`PLAN.md`) is merged as reviewed.
Each implementation phase lands as its own PR, planned and reviewed
independently before merge.
