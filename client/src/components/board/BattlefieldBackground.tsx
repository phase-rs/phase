import { type CSSProperties, useMemo, useRef } from "react";

import type { ManaColor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { BoardBackground } from "../../stores/preferencesStore.ts";
import { getDeckDominantColor } from "../../viewmodel/dominantColor.ts";
import { BATTLEFIELDS, BATTLEFIELD_MAP } from "./battlefields.ts";
import { ArenaBackground } from "./ArenaBackground.tsx";
import { PLAIN_BACKGROUND_MAP } from "./plainBackgrounds.ts";

type ResolvedBackground =
  | { kind: "arena"; color: ManaColor }
  | { kind: "image"; src: string }
  | { kind: "color"; css: string };

function pickRandomColor(): ManaColor {
  return BATTLEFIELDS[Math.floor(Math.random() * BATTLEFIELDS.length)].color;
}

export function resolveBackground(
  boardBackground: BoardBackground,
  customUrl: string,
  deckColor: ManaColor | null | undefined,
  lockedRef: React.RefObject<ManaColor | null>,
): ResolvedBackground | null {
  if (boardBackground === "none") return null;

  if (boardBackground === "custom") {
    return customUrl ? { kind: "image", src: customUrl } : null;
  }

  if (boardBackground === "random") {
    if (!lockedRef.current) {
      lockedRef.current = pickRandomColor();
    }
    return { kind: "arena", color: lockedRef.current };
  }

  if (boardBackground === "auto-wubrg") {
    // The lock is authoritative once set: the component memo short-circuits
    // the deck scan to `undefined` on every render after the first lock, so
    // this must honor an existing lock BEFORE the undefined-wait below.
    // Checking the wait first loses the lock on the very next render — any
    // gameState identity change (tap, phase tick, new frame) re-runs the
    // memo guard to undefined and the layer drops to transparent (black
    // board). (At mount the lock is fresh and re-locks, so StrictMode's dev
    // double-render is not the trigger; the first post-lock update is.)
    if (lockedRef.current) return { kind: "arena", color: lockedRef.current };

    // No seat/game state yet — hold off so the lock is never seeded from a
    // partial deck.
    if (deckColor === undefined) return null;

    // Lock in a color-matched arena on first color detection (includes full deck).
    // Colorless decks have no WUBRG color to match, so use the normal random pool.
    lockedRef.current = deckColor ?? pickRandomColor();
    return { kind: "arena", color: lockedRef.current };
  }

  const plain = PLAIN_BACKGROUND_MAP[boardBackground];
  if (plain) return { kind: "color", css: plain.css };

  const battlefield = BATTLEFIELD_MAP[boardBackground];
  if (battlefield) return { kind: "arena", color: battlefield.color };

  return null;
}

/** Escape a URL for safe use inside CSS `url("...")`. */
function cssUrl(src: string): string {
  return `url("${src.replace(/["\\]/g, (c) => `\\${c}`)}")`;
}

/** Full-screen background: animated arena, custom image, or plain color. */
export function BattlefieldBackground() {
  const boardBackground = usePreferencesStore((s) => s.boardBackground);
  const customBackgroundUrl = usePreferencesStore((s) => s.customBackgroundUrl);
  const quality = usePreferencesStore((s) => s.vfxQuality);
  const lockedRef = useRef<ManaColor | null>(null);

  const playerId = usePlayerId();
  const gameState = useGameStore((s) => s.gameState);

  // resolveBackground locks the chosen arena in lockedRef for the "random" and
  // "auto-wubrg" modes (once chosen, it sticks for the session). The lock is
  // reset by remount: GamePage keys this component on `${boardBackground}-${playerId}`,
  // so switching mode or seat unmounts and remounts it with a fresh null lockedRef.
  // That keeps render pure (no prev-value ref tracking / ref mutation during
  // render) while preserving the same reset semantics under StrictMode.

  const deckColor = useMemo(() => {
    // The dominant-color scan walks the full library + hand + battlefield, and
    // its result is consumed ONLY by the "auto-wubrg" background — and only
    // until resolveBackground locks in a color-matched arena on first detection
    // (lockedRef). For every other background mode, and on every action after
    // the lock, the result is discarded. Without this guard the scan re-ran on
    // every gameState change (mana tap, phase tick, priority pass) for nothing.
    if (boardBackground !== "auto-wubrg" || lockedRef.current) return undefined;
    if (!gameState) return undefined;
    const player = gameState.players[playerId];
    if (!player) return undefined;
    return getDeckDominantColor(
      player.library,
      player.hand,
      gameState.battlefield,
      gameState.objects,
      playerId,
    );
  }, [gameState, playerId, boardBackground]);

  const bg = resolveBackground(boardBackground, customBackgroundUrl, deckColor, lockedRef);

  // Always render the layer so right-click remains available even when no visible
  // background is configured (the "Change background" menu is most useful then).
  const style: CSSProperties =
    bg == null
      ? {}
      : bg.kind === "image"
        ? { backgroundImage: cssUrl(bg.src) }
        : bg.kind === "color"
          ? { backgroundColor: bg.css }
          : {};

  const className =
    bg?.kind === "image"
      ? "pointer-events-none fixed inset-0 bg-cover bg-center"
      : "pointer-events-none fixed inset-0";

  return (
    <div className={className} style={style}>
      {bg?.kind === "arena" && <ArenaBackground color={bg.color} quality={quality} />}
    </div>
  );
}
