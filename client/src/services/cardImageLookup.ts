import type { GameObject } from "../adapter/types.ts";

/**
 * Image-lookup descriptor for a battlefield game object.
 *
 * The frontend resolves card images via two complementary key paths in
 * `scryfall-data.json`:
 *
 *   - **`oracleId` (canonical).** When the engine attaches `printed_ref` to
 *     the object, we use Scryfall's stable per-card oracle id. This covers
 *     every printed card uniformly and sidesteps front/back-face naming
 *     asymmetry. `faceName` is then used by the Scryfall service to pick the
 *     correct entry of `card_faces` for the image (front vs back of a DFC,
 *     MDFC, transform, etc.).
 *
 *   - **`name` (legacy / fallback).** Used when the object lacks a
 *     `printed_ref` (synthesized objects, future paths) or when the caller
 *     only has a card name in scope (lobby, deck builder, hand UI for
 *     face-down cards). `faceIndex` selects the face for transformed DFCs
 *     under this path — see comment below.
 */
export interface CardImageLookup {
  oracleId?: string;
  faceName?: string;
  name: string;
  faceIndex: number;
}

/**
 * Pick the Scryfall lookup descriptor for a battlefield game object.
 *
 * Strategy:
 *
 *   1. **Object carries `printed_ref`** → return `{ oracleId, faceName }`.
 *      The engine maintains the invariant that `obj.printed_ref` always
 *      tracks the *currently displayed* face (see `printed_cards.rs:190`,
 *      where `transform_permanent` overwrites `printed_ref` to the back
 *      face's ref). The Scryfall service resolves the correct face index
 *      from `face_name` against the entry's `face_names` array — no swap
 *      needed here. Works uniformly for plain cards, DFCs, MDFCs played
 *      as either Scryfall face, and transformed permanents.
 *
 *   2. **No `printed_ref`** → legacy name-based path. Synthesized objects
 *      (emblems, generic tokens) and pre-printed_ref code paths fall here.
 *        - Not transformed → `{ name: obj.name, faceIndex: 0 }`
 *        - Transformed     → `{ name: obj.back_face.name, faceIndex: 1 }`
 *      The transformed branch swaps to `back_face.name` because the engine
 *      stashes the original front-face characteristics there, and
 *      `scripts/gen-scryfall-images.sh` indexes only front-face names. See
 *      issue #90 (The Legend of Kuruk) for context.
 */
export function cardImageLookup(
  obj: Pick<GameObject, "name" | "transformed" | "back_face" | "printed_ref">,
): CardImageLookup {
  if (obj.printed_ref) {
    return {
      oracleId: obj.printed_ref.oracle_id,
      faceName: obj.printed_ref.face_name,
      name: obj.name,
      faceIndex: obj.transformed ? 1 : 0,
    };
  }

  if (obj.transformed) {
    return {
      name: obj.back_face?.name ?? obj.name,
      faceIndex: 1,
    };
  }
  return { name: obj.name, faceIndex: 0 };
}
