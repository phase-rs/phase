import type { GameObject, Keyword } from "../adapter/types.ts";
import type { TokenSearchFilters } from "./scryfall.ts";

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
  obj: Pick<
    GameObject,
    "name" | "transformed" | "back_face" | "printed_ref" | "is_emblem" | "emblem_source"
  >,
): CardImageLookup {
  if (obj.printed_ref) {
    return {
      oracleId: obj.printed_ref.oracle_id,
      faceName: obj.printed_ref.face_name,
      name: obj.name,
      faceIndex: obj.transformed ? 1 : 0,
    };
  }

  // CR 114.5: an emblem is neither a card nor a permanent and has no
  // `printed_ref` of its own (setting one would leak the source card's
  // types/P-T into the layer system). Resolve its art from the display-only
  // `emblem_source` provenance — the card the emblem represents (e.g. Momir Vig
  // for the Momir emblem) — so an emblem's activated ability on the stack shows
  // the same art as its command-zone chip instead of a blank placeholder.
  if (obj.is_emblem && obj.emblem_source) {
    return {
      name: obj.emblem_source.name,
      oracleId: obj.emblem_source.printed_ref?.oracle_id,
      faceName: obj.emblem_source.printed_ref?.face_name,
      faceIndex: 0,
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

/**
 * Reduce an engine keyword to the base name Scryfall's `kw:` predicate
 * matches ("FirstStrike" → "first strike", `{Protection: …}` →
 * "protection"). Parameterization is dropped deliberately: art selection
 * only needs the keyword family, and a name Scryfall does not know just
 * misses its rung and degrades down the query ladder.
 */
function scryfallKeywordName(kw: Keyword): string | null {
  if (typeof kw !== "string") {
    const keys = Object.keys(kw);
    if (keys.length === 0) return null;
    if (keys[0] === "Unknown") {
      const inner = String(kw[keys[0]] ?? "").trim().toLowerCase();
      return inner || null;
    }
    return splitKeywordPascalCase(keys[0]);
  }
  return splitKeywordPascalCase(kw);
}

function splitKeywordPascalCase(raw: string): string | null {
  const name = raw
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .trim()
    .toLowerCase();
  return name || null;
}

/**
 * Build the Scryfall token-search filters for an engine game object.
 *
 * Art follows PRINTED characteristics (`base_*`), not live ones: a
 * Giant-Growth-pumped 1/1 is still a 1/1 in every printing, and an
 * anthem-granted keyword must not steer art to a natively-keyworded
 * variant. The engine populates `base_*` from the token body at creation;
 * each axis falls back to the live value for legacy paths that lack it.
 *
 * `hasAbilities` is derived purely from engine-provided fields — no rules
 * inference. A vanilla token (e.g. a 1/1 white Human from Wedding Announcement)
 * yields `hasAbilities: false`, narrowing art selection to a vanilla printing;
 * a Spirit token with flying yields `true`. See issue #502.
 *
 * Note: `abilities` has no printed counterpart on the wire, so a GRANTED
 * activated/triggered ability still flips `hasAbilities` to true (losing
 * the vanilla narrowing, as before). Keyword grants are handled: they are
 * read from `base_keywords`, so a vanilla token under an anthem keeps its
 * vanilla art.
 */
export function tokenFiltersForObject(obj: GameObject): TokenSearchFilters {
  const keywords = [
    ...new Set(
      (obj.base_keywords ?? obj.keywords ?? [])
        .map(scryfallKeywordName)
        .filter((k): k is string => k !== null),
    ),
  ];
  return {
    power: obj.base_power ?? obj.power,
    toughness: obj.base_toughness ?? obj.toughness,
    colors: obj.base_color ?? obj.color,
    subtypes: obj.card_types?.subtypes,
    keywords: keywords.length > 0 ? keywords : undefined,
    hasAbilities:
      keywords.length > 0 ||
      (obj.abilities?.length ?? 0) > 0 ||
      (obj.token_rules_text?.length ?? 0) > 0,
  };
}

/**
 * Maps a battlefield/stack `GameObject` to the props `CardImage` needs to render
 * it — resolving MDFC faces, token search filters, and token image refs. Shared
 * by every choice modal that renders object choices as card images.
 */
export function objectImageProps(obj: GameObject) {
  const { name, faceIndex, oracleId, faceName } = cardImageLookup(obj);
  const isToken = obj.display_source === "Token";
  return {
    cardName: name,
    faceIndex,
    oracleId,
    faceName,
    isToken,
    tokenFilters: isToken ? tokenFiltersForObject(obj) : undefined,
    tokenImageRef: isToken ? obj.token_image_ref : undefined,
    // Mark tapped battlefield permanents so choice modals (which render cards
    // upright, not rotated) can still surface tapped state via a {T} overlay.
    tapIndicator: obj.zone === "Battlefield" && obj.tapped,
  };
}
