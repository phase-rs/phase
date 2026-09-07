import { useEffect, useState } from "react";

import {
  deriveImageUrl,
  fetchCardImageAssetByOracleId,
  fetchTokenImageByRef,
} from "../services/scryfall.ts";
import type { DungeonCardView } from "../adapter/types.ts";

interface UseDungeonCardImageResult {
  src: string | null;
  isLoading: boolean;
}

/**
 * Resolve the printed dungeon card's image.
 *
 * This exists as its own hook rather than reusing `useCardImage` because a
 * dungeon is not a game object: it never enters the battlefield (CR 309.3 puts
 * it in the command zone), so there is no `GameObject` with a `printed_ref` to
 * feed that hook, and none of its machinery — visual packs, art-selection
 * chains, printing pickers, face-down backs, the failed-source ladder — has
 * anything to act on for a card that has exactly one relevant printing.
 *
 * Two lookup paths, because the five dungeons are not indexed uniformly by the
 * Scryfall sidecars (verified against the Scryfall API):
 *
 *   1. `scryfall-data.json` by `oracle_id`. Covers Lost Mine of Phandelver,
 *      Dungeon of the Mad Mage, Tomb of Annihilation and Baldur's Gate
 *      Wilderness, all `layout: "normal"`.
 *   2. `scryfall-token-images.json` by printing id. This is the ONLY path that
 *      resolves Undercity, which is printed as the double-faced
 *      `Undercity // The Initiative`; `gen-scryfall-images.sh` lists
 *      `double_faced_token` in its NON_PLAYABLE exclusions, so Undercity is
 *      absent from the card table entirely.
 *
 * The engine hands over both ids (`DungeonCardView`), so this tries the card
 * table first and falls back to the token table without knowing which dungeon
 * is which — no per-dungeon special case reaches the display layer.
 *
 * The result is upgraded to the `large` rung (672x936, vs `normal`'s 488x680).
 * These cards are floor plans with room names and rules text printed inside
 * each room, so the extra resolution is legibility, not polish.
 */
export function useDungeonCardImage(card: DungeonCardView): UseDungeonCardImageResult {
  const [src, setSrc] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(true);

  const { oracle_id: oracleId, scryfall_id: scryfallId, face_name: faceName } = card;

  useEffect(() => {
    let cancelled = false;
    setIsLoading(true);
    setSrc(null);

    async function resolve(): Promise<string | null> {
      try {
        const asset = await fetchCardImageAssetByOracleId(oracleId, faceName, "normal");
        if (asset.src) return asset.src;
      } catch {
        // Not in the card table — expected for Undercity, and for any dungeon
        // if the sidecar has not loaded. Fall through to the token table.
      }
      return await fetchTokenImageByRef(
        {
          scryfall_id: scryfallId,
          scryfall_oracle_id: oracleId,
          face_name: faceName,
          // `TokenImageRef.preset_id` is a token-preset concept with no meaning
          // for a dungeon card. `fetchTokenImageAssetByRef` reads only the ids
          // and the face name; this is here to satisfy the shared type.
          preset_id: "",
        },
        "normal",
      );
    }

    void resolve().then((resolved) => {
      if (cancelled) return;
      // `deriveImageUrl` returns its input unchanged for anything that is not a
      // sized Scryfall URL, so this is safe on every value that reaches it.
      setSrc(resolved ? deriveImageUrl(resolved, "large") : null);
      setIsLoading(false);
    });

    return () => {
      cancelled = true;
    };
  }, [oracleId, scryfallId, faceName]);

  return { src, isLoading };
}
