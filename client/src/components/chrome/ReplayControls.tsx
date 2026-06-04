import { memo, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { isMultiplayerMode, useGameStore } from "../../stores/gameStore.ts";
import type { GameState } from "../../adapter/types.ts";

// ── helpers ───────────────────────────────────────────────────────────────────

function checkpointLabel(state: GameState): string {
  return `T${state.turn_number}`;
}

// ── sub-components ────────────────────────────────────────────────────────────

interface TurnMarkerProps {
  state: GameState;
  index: number;
  selected: boolean;
  onSelect: (index: number) => void;
}

const TurnMarker = memo(function TurnMarker({ state, index, selected, onSelect }: TurnMarkerProps) {
  const handleClick = useCallback(() => onSelect(index), [index, onSelect]);
  return (
    <button
      onClick={handleClick}
      title={`Turn ${state.turn_number}`}
      aria-pressed={selected}
      className={[
        "flex h-7 min-w-[2.25rem] items-center justify-center rounded px-1.5 text-[11px] font-semibold transition-colors",
        selected
          ? "bg-amber-500 text-gray-900 shadow-md"
          : "bg-gray-700/70 text-gray-400 hover:bg-gray-600/80 hover:text-gray-200",
      ].join(" ")}
    >
      {checkpointLabel(state)}
    </button>
  );
});

// ── main component ────────────────────────────────────────────────────────────

/**
 * Replay controls bar — floats above the game board when turn history is
 * available. Shows a timeline of turn checkpoints and navigation arrows.
 * While in replay mode the board is read-only (dispatches are blocked in
 * the store).
 */
export const ReplayControls = memo(function ReplayControls() {
  const { t } = useTranslation("game");

  const gameMode = useGameStore((s) => s.gameMode);
  const turnCheckpoints = useGameStore((s) => s.turnCheckpoints);
  const replayMode = useGameStore((s) => s.replayMode);
  const replayIndex = useGameStore((s) => s.replayIndex);
  const currentTurn = useGameStore((s) => s.gameState?.turn_number ?? null);
  const enterReplay = useGameStore((s) => s.enterReplay);
  const exitReplay = useGameStore((s) => s.exitReplay);
  const replayTo = useGameStore((s) => s.replayTo);

  // Replay is only available in single-player modes with at least 2 checkpoints
  // (the current turn boundary and at least one prior turn).
  const available = !isMultiplayerMode(gameMode) && turnCheckpoints.length >= 2;

  const handlePrev = useCallback(() => {
    if (replayIndex == null) return;
    replayTo(replayIndex - 1);
  }, [replayIndex, replayTo]);

  const handleNext = useCallback(() => {
    if (replayIndex == null) return;
    replayTo(replayIndex + 1);
  }, [replayIndex, replayTo]);

  if (!available) return null;

  if (!replayMode) {
    return (
      <div className="pointer-events-auto flex items-center">
        <button
          onClick={() => enterReplay()}
          title={t("replay.viewHistory")}
          className="flex items-center gap-1.5 rounded-md bg-gray-800/80 px-2.5 py-1.5 text-[11px] font-medium text-gray-400 transition-colors hover:bg-gray-700/80 hover:text-gray-200"
        >
          <HistoryIcon />
          {t("replay.viewHistory")}
        </button>
      </div>
    );
  }

  const canPrev = replayIndex != null && replayIndex > 0;
  const canNext = replayIndex != null && replayIndex < turnCheckpoints.length - 1;

  return (
    <div
      role="region"
      aria-label={t("replay.replayBanner", { turn: turnCheckpoints[replayIndex ?? 0]?.turn_number ?? "?" })}
      className="pointer-events-auto flex w-full flex-col gap-1.5"
    >
      {/* Banner */}
      <div className="flex items-center justify-center gap-2 rounded-md bg-amber-900/70 px-3 py-1 text-[11px] font-medium text-amber-300 ring-1 ring-amber-700/50">
        <span>
          {t("replay.replayBanner", {
            turn: turnCheckpoints[replayIndex ?? 0]?.turn_number ?? "?",
          })}
        </span>
        {currentTurn != null && (
          <span className="text-amber-500/70">(live: T{currentTurn})</span>
        )}
      </div>

      {/* Timeline + navigation */}
      <div className="flex items-center gap-2">
        <NavButton onClick={handlePrev} disabled={!canPrev} direction="prev" />

        <div className="flex flex-1 items-center gap-1 overflow-x-auto py-0.5 scrollbar-none">
          {turnCheckpoints.map((cp, i) => (
            <TurnMarker
              key={i}
              state={cp}
              index={i}
              selected={i === replayIndex}
              onSelect={replayTo}
            />
          ))}
        </div>

        <NavButton onClick={handleNext} disabled={!canNext} direction="next" />

        <button
          onClick={exitReplay}
          className="ml-1 flex items-center gap-1 rounded-md bg-gray-700/80 px-2.5 py-1.5 text-[11px] font-medium text-gray-300 transition-colors hover:bg-gray-600/80 hover:text-white"
        >
          {t("replay.exitReplay")}
        </button>
      </div>
    </div>
  );
});

// ── icon helpers ──────────────────────────────────────────────────────────────

function HistoryIcon() {
  return (
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3.5 w-3.5">
      <path fillRule="evenodd" d="M8 14A6 6 0 1 1 2.21 5.257a.75.75 0 1 1 1.146.964A4.5 4.5 0 1 0 8 3.5a4.484 4.484 0 0 0-3.06 1.19l.81.81A.75.75 0 0 1 5.25 6.75h-3a.75.75 0 0 1-.75-.75v-3a.75.75 0 0 1 1.28-.53l.72.72A6 6 0 0 1 8 2a6 6 0 0 1 0 12Zm.75-8.25a.75.75 0 0 0-1.5 0v2.69l-1.22 1.22a.75.75 0 1 0 1.06 1.06l1.5-1.5a.75.75 0 0 0 .22-.53V5.75Z" clipRule="evenodd" />
    </svg>
  );
}

interface NavButtonProps {
  onClick: () => void;
  disabled: boolean;
  direction: "prev" | "next";
}

function NavButton({ onClick, disabled, direction }: NavButtonProps) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      aria-label={direction === "prev" ? "Previous turn" : "Next turn"}
      className="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded bg-gray-700/70 text-gray-400 transition-colors hover:bg-gray-600/80 hover:text-gray-200 disabled:cursor-not-allowed disabled:opacity-30"
    >
      {direction === "prev" ? (
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3.5 w-3.5">
          <path fillRule="evenodd" d="M9.78 4.22a.75.75 0 0 1 0 1.06L7.06 8l2.72 2.72a.75.75 0 1 1-1.06 1.06L5.47 8.53a.75.75 0 0 1 0-1.06l3.25-3.25a.75.75 0 0 1 1.06 0Z" clipRule="evenodd" />
        </svg>
      ) : (
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3.5 w-3.5">
          <path fillRule="evenodd" d="M6.22 4.22a.75.75 0 0 1 1.06 0l3.25 3.25a.75.75 0 0 1 0 1.06L7.28 11.78a.75.75 0 0 1-1.06-1.06L9.44 8 6.22 4.78a.75.75 0 0 1 0-1.06Z" clipRule="evenodd" />
        </svg>
      )}
    </button>
  );
}
