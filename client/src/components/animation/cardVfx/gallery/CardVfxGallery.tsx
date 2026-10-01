import { type ReactNode, useCallback, useEffect, useRef, useState } from "react";

import type { GameState } from "../../../../adapter/types.ts";
import type { CardAnimationStyle, VfxQuality } from "../../../../animation/types.ts";
import { processRemoteUpdate } from "../../../../game/dispatch.ts";
import { useResolvedGridRows } from "../../../../hooks/useResolvedGridRows.ts";
import { useAnimationStore } from "../../../../stores/animationStore.ts";
import { useGameStore } from "../../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../../stores/preferencesStore.ts";
import { buildLegalActionsResult } from "../../../../test/factories/gameStateFactory.ts";
import { GameBoard } from "../../../board/GameBoard.tsx";
import { OpponentHand } from "../../../hand/OpponentHand.tsx";
import { PlayerHand } from "../../../hand/PlayerHand.tsx";
import { OpponentHud } from "../../../hud/OpponentHud.tsx";
import { PlayerHud } from "../../../hud/PlayerHud.tsx";
import { StackDisplay } from "../../../stack/StackDisplay.tsx";
import { ExilePile } from "../../../zone/ExilePile.tsx";
import { GraveyardPile } from "../../../zone/GraveyardPile.tsx";
import { LibraryPile } from "../../../zone/LibraryPile.tsx";
import { AnimationOverlay } from "../../AnimationOverlay.tsx";
import { GALLERY_SCENARIOS, galleryBoard } from "./galleryScenarios.ts";

export type GalleryScenarioId = keyof typeof GALLERY_SCENARIOS;

interface CardVfxGalleryProps {
  scenario: GalleryScenarioId;
  background?: ReactNode;
}

const PILE_SIZE = { width: "clamp(45px, 4.5vw, 70px)", height: "clamp(63px, 6.3vw, 98px)" };
const STYLES: [CardAnimationStyle, string][] = [["webgl", "New"], ["classic", "Classic"]];
const QUALITIES: VfxQuality[] = ["full", "reduced", "minimal"];
const SPEEDS = [0.5, 1, 2];
/** Time for a freshly committed board to lay out before its first update animates. */
const SETTLE_MS = 400;
const noop = () => {};

/** Commits `state` as the engine would: the next snapshot, applied at once. */
function commit(state: GameState) {
  const seq = useGameStore.getState().lastCommittedSeq + 1;
  useGameStore.getState().commitEngineSnapshot({ state, legalResult: buildLegalActionsResult(), seq });
}

/**
 * A two-player board that replays one gallery scenario through the app's own
 * pipeline: each engine update's events go to `processRemoteUpdate`, which
 * animates them and then commits the state they leave, as a live game does.
 */
export function CardVfxGallery({ scenario, background }: CardVfxGalleryProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const gridTemplateRows = useResolvedGridRows();
  const hasBoard = useGameStore((s) => s.gameState !== null);
  const [playing, setPlaying] = useState(false);
  const { title, description, batches } = GALLERY_SCENARIOS[scenario];

  const play = useCallback(async () => {
    setPlaying(true);
    useAnimationStore.getState().clearQueue();
    let state = galleryBoard();
    commit(state);
    await new Promise((resolve) => setTimeout(resolve, SETTLE_MS));
    for (const batch of batches) {
      const next = structuredClone(state);
      const events = batch(next);
      const seq = useGameStore.getState().lastCommittedSeq + 1;
      await processRemoteUpdate({ state: next, legalResult: buildLegalActionsResult(), seq }, events);
      state = next;
    }
    setPlaying(false);
  }, [batches]);

  useEffect(() => {
    void play();
  }, [play]);

  return (
    <div
      ref={containerRef}
      className="game-no-select relative h-[100dvh] w-full overflow-hidden bg-gray-950 contain-paint"
    >
      {background}
      {hasBoard && (
        <>
          <StackDisplay effectiveMultiplayerBoardLayout="focused" />
          <div className="relative grid h-full min-w-0" style={{ gridTemplateRows, gridTemplateColumns: "1fr" }}>
            <div className="relative z-20 grid min-w-0 grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)]">
              <div aria-hidden />
              <div className="min-w-0">
                <OpponentHand />
              </div>
              <div className="flex items-start justify-self-end gap-1.5 px-1 py-1">
                <ExilePile playerId={1} size={PILE_SIZE} onClick={noop} />
                <LibraryPile playerId={1} size={PILE_SIZE} />
                <GraveyardPile playerId={1} size={PILE_SIZE} onClick={noop} />
              </div>
            </div>
            <div className="relative z-30 flex min-h-0 min-w-0 flex-col">
              <GameBoard effectiveMultiplayerBoardLayout="focused" oppHud={<OpponentHud />} playerHud={<PlayerHud />} />
            </div>
            <div className="relative min-w-0 self-end overflow-visible" style={{ height: "var(--game-player-row-height)" }}>
              <div className="flex items-end justify-center">
                <PlayerHand />
              </div>
              <div className="absolute bottom-0 left-0 z-10 flex items-end gap-2 p-3">
                <ExilePile playerId={0} size={PILE_SIZE} onClick={noop} />
                <LibraryPile playerId={0} size={PILE_SIZE} />
                <GraveyardPile playerId={0} size={PILE_SIZE} onClick={noop} />
              </div>
            </div>
          </div>
          <AnimationOverlay containerRef={containerRef} />
        </>
      )}
      <GalleryToolbar title={title} description={description} playing={playing} onReplay={() => void play()} />
    </div>
  );
}

function GalleryToolbar({
  title,
  description,
  playing,
  onReplay,
}: {
  title: string;
  description: string;
  playing: boolean;
  onReplay: () => void;
}) {
  const style = usePreferencesStore((s) => s.cardAnimationStyle);
  const quality = usePreferencesStore((s) => s.vfxQuality);
  const speed = usePreferencesStore((s) => s.animationSpeedMultiplier);
  const { setCardAnimationStyle, setVfxQuality, setAnimationSpeedMultiplier } = usePreferencesStore.getState();
  const select = "rounded bg-white/10 px-2 py-1 text-xs text-white";

  return (
    <div className="absolute left-3 top-1/2 z-[60] w-64 -translate-y-1/2 space-y-3 rounded-lg bg-black/70 p-3 text-white shadow-lg backdrop-blur">
      <div>
        <div className="text-sm font-semibold">{title}</div>
        <div className="mt-1 text-xs text-white/70">{description}</div>
      </div>
      <div className="flex gap-1">
        {STYLES.map(([value, label]) => (
          <button
            key={value}
            type="button"
            className={`flex-1 rounded px-2 py-1 text-xs ${style === value ? "bg-sky-600" : "bg-white/10"}`}
            onClick={() => setCardAnimationStyle(value)}
          >
            {label}
          </button>
        ))}
      </div>
      <div className="flex items-center gap-2 text-xs">
        <label className="flex items-center gap-1">
          Quality
          <select className={select} value={quality} onChange={(e) => setVfxQuality(e.target.value as VfxQuality)}>
            {QUALITIES.map((q) => (
              <option key={q} value={q}>
                {q}
              </option>
            ))}
          </select>
        </label>
        <label className="flex items-center gap-1">
          Speed
          <select
            className={select}
            value={speed}
            onChange={(e) => setAnimationSpeedMultiplier(Number(e.target.value))}
          >
            {SPEEDS.map((s) => (
              <option key={s} value={s}>
                {s}×
              </option>
            ))}
          </select>
        </label>
      </div>
      <button
        type="button"
        className="w-full rounded bg-sky-600 px-2 py-1.5 text-sm font-semibold disabled:opacity-50"
        disabled={playing}
        onClick={onReplay}
      >
        {playing ? "Playing…" : "Replay"}
      </button>
    </div>
  );
}
