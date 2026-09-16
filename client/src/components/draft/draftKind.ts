/**
 * Draft Kind — the pod-draft kind union, its deep-link slug, and its player-facing
 * labels.
 *
 * LEAF MODULE BY CONSTRUCTION. It imports one type from the adapter and one type
 * from `i18next`, and it must never import a store — not even a type. The landing
 * page needs the slug and the labels but needs no store: routing its `lazy()` chunk
 * through `draftPodStore` pulls `multiplayerDraftStore -> draftPodHostAdapter ->
 * p2p-draft-host -> network/connection` plus the game loop onto a page that renders
 * four tiles. `verbatimModuleSyntax` erases `import type`, but nothing in the lint
 * config stops a later edit from dropping the `type` keyword, so the safe invariant
 * is "no store is named here at all" rather than "the store edge is type-only".
 */

import type { TFunction } from "i18next";

import type { DraftKind as CoreDraftKind } from "../../adapter/draft-adapter";

/** The pod-hostable draft kinds. `Quick` is the solo/AI path and has no pod. */
export type DraftKind = Exclude<CoreDraftKind, "Quick">;

/** `?kind=` value on `/draft-pod` that deep-links pod setup into a Commander draft.
 *  Mirrors DraftPage's `?mode=sealed|cube` entry-point convention. One symbol so the
 *  landing tile that writes the URL and the pod page that reads it cannot drift. */
export const COMMANDER_DRAFT_ENTRY = "commander";

/** `?kind=` value on `/draft-pod` that deep-links pod setup into a Winston draft.
 *  Same contract as `COMMANDER_DRAFT_ENTRY`: the landing tile writes it, the pod page
 *  reads it, and neither spells the slug itself. */
export const WINSTON_DRAFT_ENTRY = "winston";

/** Every `?kind=` slug and the kind it deep-links.
 *
 *  One map rather than one boolean per slug at the reading site: the pod page's entry
 *  effect then folds this instead of growing a parallel `<kind>Requested` const and a
 *  parallel `useEffect` per kind. `satisfies` keeps the values inside `DraftKind`
 *  without widening the key type, so `draftKindForEntry` stays exhaustive over the
 *  slugs actually published here. Not every kind has a slug — the kinds reachable
 *  from the plain `/draft-pod` radios need none. */
export const DRAFT_KIND_ENTRIES = {
  [COMMANDER_DRAFT_ENTRY]: "CommanderDraft",
  [WINSTON_DRAFT_ENTRY]: "Winston",
} as const satisfies Record<string, DraftKind>;

/** The kind a `?kind=` slug deep-links, or `null` for an absent or unknown slug.
 *  An unknown slug is deliberately not an error: a stale bookmark falls back to the
 *  plain pod setup rather than failing the route. */
export function draftKindForEntry(slug: string | null): DraftKind | null {
  if (slug === null) return null;
  // Widened to a string index for the lookup only. The map itself keeps its literal
  // key type, so `DRAFT_KIND_ENTRIES.commander` is still a checked property access
  // everywhere else; this local alias is what lets an UNTRUSTED URL string be looked
  // up without an `as` cast on the slug.
  const entries: Readonly<Record<string, DraftKind>> = DRAFT_KIND_ENTRIES;
  return entries[slug] ?? null;
}

/** Human-readable label for every `DraftKind`, resolved from the `draft` namespace.
 *
 *  Single authority, colocated with the union it is keyed on. Two surfaces render a
 *  bare kind to the player — the landing page's resume card and the pod lobby header —
 *  and both interpolate it into a sentence, so a raw enum reads "CommanderDraft Pod".
 *  Two independent maps for one fact would let those surfaces disagree.
 *
 *  Total over `DraftKind`: a future kind is a TS2741 at this literal rather than a
 *  raw enum leaking into copy. Values are already-resolved strings because
 *  `react-i18next.d.ts` types `t`'s key against the `en` catalog, so a `t(variable)`
 *  lookup would not typecheck. */
export function draftKindLabels(t: TFunction<"draft">): Record<DraftKind, string> {
  return {
    Premier: t("podSetup.kindPremier"),
    Traditional: t("podSetup.kindTraditional"),
    Sealed: t("podSetup.kindSealed"),
    CommanderDraft: t("podSetup.kindCommanderDraft"),
    Winston: t("podSetup.kindWinston"),
  };
}
