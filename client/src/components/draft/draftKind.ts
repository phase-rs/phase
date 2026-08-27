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
  };
}
