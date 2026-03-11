import { useMemo, useRef } from "react";

import type { ManaColor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { BoardBackground } from "../../stores/preferencesStore.ts";
import { getDominantManaColor } from "../../viewmodel/dominantColor.ts";
import { BATTLEFIELDS, BATTLEFIELD_MAP, getRandomBattlefield } from "./battlefields.ts";

function pickRandomImage(): string {
  return BATTLEFIELDS[Math.floor(Math.random() * BATTLEFIELDS.length)].image;
}

function resolveBattlefieldImage(
  boardBackground: BoardBackground,
  dominantColor: ManaColor | null,
  colorMatchedRef: React.RefObject<string | null>,
  fallbackRef: React.RefObject<string | null>,
): string | null {
  if (boardBackground === "none") return null;

  if (boardBackground === "auto-wubrg") {
    // Once we have a dominant color, lock in a color-matched image
    if (dominantColor && !colorMatchedRef.current) {
      colorMatchedRef.current = getRandomBattlefield(dominantColor).image;
    }
    if (colorMatchedRef.current) return colorMatchedRef.current;

    // No lands played yet — show a random background instead of nothing
    if (!fallbackRef.current) {
      fallbackRef.current = pickRandomImage();
    }
    return fallbackRef.current;
  }

  return BATTLEFIELD_MAP[boardBackground]?.image ?? null;
}

/** Full-screen battlefield background image. */
export function BattlefieldBackground() {
  const boardBackground = usePreferencesStore((s) => s.boardBackground);
  const colorMatchedRef = useRef<string | null>(null);
  const fallbackRef = useRef<string | null>(null);

  const playerId = usePlayerId();
  const gameState = useGameStore((s) => s.gameState);
  const dominantColor = useMemo(() => {
    if (!gameState) return null;
    return getDominantManaColor(gameState.battlefield, gameState.objects, playerId);
  }, [gameState, playerId]);

  const bgImage = resolveBattlefieldImage(boardBackground, dominantColor, colorMatchedRef, fallbackRef);

  if (!bgImage) return null;

  return (
    <div
      className="pointer-events-none fixed inset-0 bg-cover bg-center"
      style={{ backgroundImage: `url(${bgImage})` }}
    />
  );
}
