/**
 * Every draft kind, as one runtime tuple the type is DERIVED from.
 *
 * The tuple exists because a type-guard body is not checked against its target
 * union: `function isDraftKind(v): v is DraftKind` compiles whether the body
 * enumerates six kinds or two, so a duplicated enumeration beside the union
 * goes silently narrow the moment a kind is added — and a persisted session of
 * the new kind is then discarded on resume with no error anywhere. Folding the
 * guard over this tuple makes the enumeration the type, so the class cannot
 * recur at the next widening. Never restate these members anywhere else;
 * derive from `DRAFT_KINDS`.
 *
 * No imports: `constants/storage.ts` and `services/draftDeckAutosave.ts` take
 * these runtime values from here rather than from `./draft-adapter`, because
 * `vi.mock` of `draft-adapter` replaces the whole module and some test
 * factories do not re-export `DRAFT_KINDS`.
 */
// @sync-with: crates/draft-core/src/types.rs `DraftKind::ALL`
export const DRAFT_KINDS = [
  "Quick",
  "Premier",
  "Traditional",
  "Sealed",
  "CommanderDraft",
  "Winston",
] as const;

export type DraftKind = (typeof DRAFT_KINDS)[number];

/** The set code a solo cube draft runs under; only this tells it apart from a solo Quick draft. */
export const CUSTOM_CUBE_SET_CODE = "custom-cube";
