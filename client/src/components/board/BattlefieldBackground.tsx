import { useMemo, useRef } from "react";

import type { ManaColor } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { BoardBackground } from "../../stores/preferencesStore.ts";
import { getDominantManaColor } from "../../viewmodel/dominantColor.ts";
import { BATTLEFIELD_MAP, getRandomBattlefield } from "./battlefields.ts";

function resolveBattlefieldImage(
  boardBackground: BoardBackground,
  dominantColor: ManaColor | null,
  autoRef: React.RefObject<string | null>,
): string | null {
  if (boardBackground === "none") return null;

  if (boardBackground === "auto-wubrg") {
    if (!dominantColor) return null;
    if (!autoRef.current) {
      autoRef.current = getRandomBattlefield(dominantColor).image;
    }
    return autoRef.current;
  }

  return BATTLEFIELD_MAP[boardBackground]?.image ?? null;
}

/** Full-screen battlefield background image. */
export function BattlefieldBackground() {
  const boardBackground = usePreferencesStore((s) => s.boardBackground);
  const autoImageRef = useRef<string | null>(null);

  const gameState = useGameStore((s) => s.gameState);
  const dominantColor = useMemo(() => {
    if (!gameState) return null;
    return getDominantManaColor(gameState.battlefield, gameState.objects, 0);
  }, [gameState]);

  const bgImage = resolveBattlefieldImage(boardBackground, dominantColor, autoImageRef);

  if (!bgImage) return null;

  return (
    <div
      className="pointer-events-none fixed inset-0 bg-cover bg-center"
      style={{ backgroundImage: `url(${bgImage})` }}
    />
  );
}
