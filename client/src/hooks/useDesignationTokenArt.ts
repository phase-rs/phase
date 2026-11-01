import { useEffect, useState } from "react";

import { fetchTokenImageAssetByRef, fetchTokenImageUrl } from "../services/scryfall.ts";

/**
 * Player/game designations with official Scryfall token art. Single-face
 * tokens resolve by name through the `token:` sidecar keys (plus the live
 * Scryfall ladder when the sidecar misses); double-faced tokens resolve by
 * printing id + face name through `scryfall-token-images.json` only — that
 * table is the sole carrier of `double_faced_token` layouts, and the name
 * path cannot address a non-front face.
 */
export type DesignationArtKind =
  | "monarch"
  | "initiative"
  | "cityBlessing"
  | "radiation"
  | "ring"
  | "day"
  | "night";

/** Single-face designation tokens, keyed by exact Scryfall token name. */
const NAME_LOOKUPS: Partial<Record<DesignationArtKind, string>> = {
  monarch: "The Monarch",
  cityBlessing: "City's Blessing",
  radiation: "Radiation",
};

interface TokenRefLookup {
  scryfall_id: string;
  face_name: string;
}

/**
 * Double-faced designation tokens. Scryfall printing ids are immutable
 * external identifiers, so pinning them here cannot drift the way a name
 * can — but the Undercity id duplicates the engine's
 * `dungeon::card_ref(Undercity).scryfall_id`, which stays the source of
 * truth if the pinned printing ever changes (its art twins are identical
 * at every rung, per that site's comment).
 */
const REF_LOOKUPS: Partial<Record<DesignationArtKind, TokenRefLookup>> = {
  // Back face of `Undercity // The Initiative` (CLB token #020): the marker
  // a player physically holds while they have the initiative. Deliberately
  // the Initiative face, never the dungeon face — the initiative badge must
  // read as a separate designation from the dungeon display so attackers can
  // see who to hit to take it back.
  initiative: {
    scryfall_id: "2c65185b-6cf0-451d-985e-56aa45d9a57d",
    face_name: "The Initiative",
  },
  // Front face of `The Ring // The Ring Tempts You` (LTR token H13).
  ring: {
    scryfall_id: "7215460e-8c06-47d0-94e5-d1832d0218af",
    face_name: "The Ring",
  },
  // Faces of `Day // Night` (MID token #019).
  day: {
    scryfall_id: "9c0f7843-4cbb-4d0f-8887-ec823a9238da",
    face_name: "Day",
  },
  night: {
    scryfall_id: "9c0f7843-4cbb-4d0f-8887-ec823a9238da",
    face_name: "Night",
  },
};

async function resolveDesignationArt(kind: DesignationArtKind): Promise<string | null> {
  const name = NAME_LOOKUPS[kind];
  if (name !== undefined) {
    try {
      return await fetchTokenImageUrl(name, "art_crop");
    } catch {
      return null;
    }
  }
  const ref = REF_LOOKUPS[kind];
  if (ref === undefined) return null;
  try {
    // `preset_id` is a token-preset concept with no meaning for a
    // designation; `fetchTokenImageAssetByRef` reads only the ids and the
    // face name. Same dummy the dungeon art hook passes.
    const asset = await fetchTokenImageAssetByRef(
      { scryfall_id: ref.scryfall_id, face_name: ref.face_name, preset_id: "" },
      "art_crop",
    );
    return asset?.src ?? null;
  } catch {
    return null;
  }
}

/**
 * Resolve a designation token's `art_crop` image URL, or `null` while
 * loading / when art is unavailable (offline, sidecar missing). Callers
 * render their glyph fallback on `null` — art is decoration; the badge's
 * accessible name never depends on it.
 */
export function useDesignationTokenArt(kind: DesignationArtKind): string | null {
  const [src, setSrc] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setSrc(null);
    void resolveDesignationArt(kind)
      .catch(() => null)
      .then((resolved) => {
        if (!cancelled) setSrc(resolved);
      });
    return () => {
      cancelled = true;
    };
  }, [kind]);

  return src;
}
