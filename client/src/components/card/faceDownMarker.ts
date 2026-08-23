import type { FaceDownCause, TokenImageRef } from "../../adapter/types.ts";

/**
 * The marker token Wizards prints for each face-down family.
 *
 * Paper play uses these as the required "what ability caused them to be face
 * down" reminder (Duskmourn rulings, 2024-09-20), and the engine already tells
 * us the cause. Mapping the cause onto a printing is a display decision, which
 * is why the ids live here and not in the engine: four rules-level causes share
 * three printed tokens, and one cause has no token at all.
 *
 * Oracle ids are used rather than a single printing's Scryfall id so the lookup
 * survives a reprint — `fetchTokenImageByRef` falls back to the oracle key that
 * `scryfall-token-images.json` already indexes for all three.
 */
const MARKERS: Partial<Record<FaceDownCause, TokenImageRef>> = {
  // https://scryfall.com/card/tfrf/4/manifest — also used for manifest dread,
  // which is the same keyword action with a different card-selection step.
  Manifest: {
    scryfall_id: "",
    scryfall_oracle_id: "f4f184ef-f456-47d8-9012-095629a5ea4d",
    face_name: "manifest",
    preset_id: "face-down-manifest",
  },
  // https://scryfall.com/card/tdtk/7/morph — megamorph shares it.
  Morph: {
    scryfall_id: "",
    scryfall_oracle_id: "8f92f8d7-ec89-426f-86dc-fbc259eb5559",
    face_name: "morph",
    preset_id: "face-down-morph",
  },
  // https://scryfall.com/card/tmkm/21/a-mysterious-creature — cloak and
  // disguise are different rules (CR 701.58a vs CR 702.168a) with one printing.
  Cloak: {
    scryfall_id: "",
    scryfall_oracle_id: "6481a124-6859-4f02-9fd3-b1302528dd2e",
    face_name: "a mysterious creature",
    preset_id: "face-down-cloak",
  },
  Disguise: {
    scryfall_id: "",
    scryfall_oracle_id: "6481a124-6859-4f02-9fd3-b1302528dd2e",
    face_name: "a mysterious creature",
    preset_id: "face-down-cloak",
  },
  // `TurnedFaceDown` (Ixidron class) is deliberately absent: no marker token is
  // printed for it, so it keeps the generic card back.
};

/** Printed token names, for the tile's name bar and the preview caption. */
const MARKER_NAMES: Partial<Record<FaceDownCause, string>> = {
  Manifest: "Manifest",
  Morph: "Morph",
  Cloak: "A Mysterious Creature",
  Disguise: "A Mysterious Creature",
};

/**
 * The printed marker token's NAME for a face-down permanent, or `null` when
 * none applies. Shown on the battlefield tile instead of the generic
 * "Face-down card" label.
 */
export function faceDownMarkerName(
  faceDown: boolean,
  cause: FaceDownCause | null | undefined,
): string | null {
  if (!faceDown || !cause) return null;
  return MARKER_NAMES[cause] ?? null;
}

/**
 * The marker printing for a face-down permanent, or `null` when none applies —
 * the permanent is face up, the engine did not record a cause (older saves), or
 * the cause has no printed token.
 */
export function faceDownMarkerRef(
  faceDown: boolean,
  cause: FaceDownCause | null | undefined,
): TokenImageRef | null {
  if (!faceDown || !cause) return null;
  return MARKERS[cause] ?? null;
}
