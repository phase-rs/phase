import { type CSSProperties, useMemo, useRef } from "react";

import type { ManaColor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { BoardBackground } from "../../stores/preferencesStore.ts";
import { getDeckDominantColor } from "../../viewmodel/dominantColor.ts";
import { BATTLEFIELDS, BATTLEFIELD_MAP, getRandomBattlefield } from "./battlefields.ts";
import { PLAIN_BACKGROUND_MAP } from "./plainBackgrounds.ts";

type ResolvedBackground =
  | { kind: "image"; src: string }
  | { kind: "color"; css: string };

function pickRandomImage(): string {
  return BATTLEFIELDS[Math.floor(Math.random() * BATTLEFIELDS.length)].image;
}

function resolveBackground(
  boardBackground: BoardBackground,
  customUrl: string,
  deckColor: ManaColor | null,
  lockedRef: React.RefObject<string | null>,
): ResolvedBackground | null {
  if (boardBackground === "none") return null;

  if (boardBackground === "custom") {
    return customUrl ? { kind: "image", src: customUrl } : null;
  }

  if (boardBackground === "random") {
    if (!lockedRef.current) {
      lockedRef.current = pickRandomImage();
    }
    return { kind: "image", src: lockedRef.current };
  }

  if (boardBackground === "auto-wubrg") {
    // Lock in a color-matched image on first color detection (includes full deck)
    if (deckColor && !lockedRef.current) {
      lockedRef.current = getRandomBattlefield(deckColor).image;
    }
    return lockedRef.current ? { kind: "image", src: lockedRef.current } : null;
  }

  const plain = PLAIN_BACKGROUND_MAP[boardBackground];
  if (plain) return { kind: "color", css: plain.css };

  const battlefield = BATTLEFIELD_MAP[boardBackground];
  if (battlefield) return { kind: "image", src: battlefield.image };

  return null;
}

/** Escape a URL for safe use inside CSS `url("...")`. */
function cssUrl(src: string): string {
  return `url("${src.replace(/["\\]/g, (c) => `\\${c}`)}")`;
}

/** Full-screen battlefield background — either art image or plain color. */
export function BattlefieldBackground() {
  const boardBackground = usePreferencesStore((s) => s.boardBackground);
  const customBackgroundUrl = usePreferencesStore((s) => s.customBackgroundUrl);
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

  const bg = resolveBackground(boardBackground, customBackgroundUrl, deckColor, lockedRef);

  // Always render the layer so right-click remains available even when no visible
  // background is configured (the "Change background" menu is most useful then).
  const style: CSSProperties =
    bg == null
      ? {}
      : bg.kind === "image"
        ? { backgroundImage: cssUrl(bg.src) }
        : { backgroundColor: bg.css };

  const className =
    bg?.kind === "image"
      ? "pointer-events-none fixed inset-0 bg-cover bg-center"
      : "pointer-events-none fixed inset-0";

  return <div className={className} style={style} />;
}
