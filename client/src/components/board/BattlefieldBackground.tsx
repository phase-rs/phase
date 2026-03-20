import { useMemo, useRef } from "react";

import type { ManaColor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { BoardBackground } from "../../stores/preferencesStore.ts";
import { getDeckDominantColor } from "../../viewmodel/dominantColor.ts";
import { BATTLEFIELDS, BATTLEFIELD_MAP, getRandomBattlefield } from "./battlefields.ts";

function pickRandomImage(): string {
  return BATTLEFIELDS[Math.floor(Math.random() * BATTLEFIELDS.length)].image;
}

function resolveBattlefieldImage(
  boardBackground: BoardBackground,
  deckColor: ManaColor | null,
  lockedRef: React.RefObject<string | null>,
): string | null {
  if (boardBackground === "none") return null;

  if (boardBackground === "random") {
    if (!lockedRef.current) {
      lockedRef.current = pickRandomImage();
    }
    return lockedRef.current;
  }

  if (boardBackground === "auto-wubrg") {
    // Lock in a color-matched image on first color detection (includes full deck)
    if (deckColor && !lockedRef.current) {
      lockedRef.current = getRandomBattlefield(deckColor).image;
    }
    return lockedRef.current;
  }

  return BATTLEFIELD_MAP[boardBackground]?.image ?? null;
}

/** Full-screen battlefield background image. */
export function BattlefieldBackground() {
  const boardBackground = usePreferencesStore((s) => s.boardBackground);
  const lockedRef = useRef<string | null>(null);

  const playerId = usePlayerId();
  const gameState = useGameStore((s) => s.gameState);
  const deckColor = useMemo(() => {
    if (!gameState) return null;
    const player = gameState.players[playerId];
    if (!player) return null;
    return getDeckDominantColor(
      player.library,
      player.hand,
      gameState.battlefield,
      gameState.objects,
      playerId,
    );
  }, [gameState, playerId]);

  const bgImage = resolveBattlefieldImage(boardBackground, deckColor, lockedRef);

  if (!bgImage) return null;

  return (
    <div
      className="pointer-events-none fixed inset-0 bg-cover bg-center"
      style={{ backgroundImage: `url(${bgImage})` }}
    />
  );
}
