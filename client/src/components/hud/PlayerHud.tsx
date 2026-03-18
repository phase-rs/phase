import { useCallback } from "react";

import { audioManager } from "../../audio/AudioManager.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";
import { PhaseIndicatorLeft, PhaseIndicatorRight } from "../controls/PhaseStopBar.tsx";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";

export function PlayerHud() {
  const masterMuted = usePreferencesStore((s) => s.masterMuted);
  const setMasterMuted = usePreferencesStore((s) => s.setMasterMuted);
  const playerId = usePlayerId();
  const isMyTurn = useGameStore((s) => s.gameState?.active_player === playerId);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  const isHumanTargetSelection =
    (waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection")
    && waitingFor.data.player === playerId;
  const isValidTarget = isHumanTargetSelection && (waitingFor.data.selection?.current_legal_targets ?? []).some(
    (target) => "Player" in target && target.Player === playerId,
  );

  const handleTargetClick = useCallback(() => {
    if (isValidTarget) {
      dispatch({ type: "ChooseTarget", data: { target: { Player: playerId } } });
    }
  }, [isValidTarget, dispatch, playerId]);

  const pillClass = isValidTarget
    ? "bg-black/50 ring-[3px] ring-cyan-400 shadow-[0_0_20px_rgba(34,211,238,0.6),0_0_8px_rgba(34,211,238,0.4)] cursor-pointer"
    : isMyTurn
      ? "bg-black/50 ring-[3px] ring-emerald-400 shadow-[0_0_20px_rgba(52,211,153,0.5),0_0_6px_rgba(52,211,153,0.4)]"
      : "bg-black/50";

  return (
    <div
      data-player-hud={playerId}
      className="relative z-20 flex shrink-0 max-w-[calc(100vw-0.75rem)] flex-col items-center justify-center gap-2 px-2 py-1 sm:max-w-none sm:flex-row sm:flex-nowrap sm:gap-3"
    >
      <div className="hidden sm:block">
        <PhaseIndicatorLeft />
      </div>
      <div
        onClick={handleTargetClick}
        className={`flex min-w-0 flex-wrap items-center justify-center gap-1.5 rounded-full px-2.5 py-1 transition-all duration-300 sm:flex-nowrap sm:gap-2 sm:px-3 ${pillClass}`}
      >
        <LifeTotal playerId={playerId} size="lg" hideLabel />
        <span className="text-[11px] font-medium uppercase tracking-[0.18em] text-gray-500">
          P{playerId + 1}
        </span>
        <ManaPoolSummary playerId={playerId} />
        <button
          onClick={() => {
            const willUnmute = masterMuted;
            setMasterMuted(!masterMuted);
            if (willUnmute) audioManager.ensurePlayback();
          }}
          className={`rounded-full p-1.5 transition-colors hover:bg-white/10 hover:text-gray-300 ${
            masterMuted ? "text-red-400" : "text-gray-500"
          }`}
          aria-label={masterMuted ? "Unmute audio" : "Mute audio"}
        >
          {masterMuted ? (
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 20 20"
              fill="currentColor"
              className="h-4 w-4"
            >
              <path d="M9.547 3.062A.75.75 0 0 1 10 3.75v12.5a.75.75 0 0 1-1.264.546L5.203 13.5H2.667a.75.75 0 0 1-.7-.48A6.985 6.985 0 0 1 1.5 10c0-.85.151-1.665.429-2.42a.75.75 0 0 1 .737-.58h2.499l3.533-3.296a.75.75 0 0 1 .849-.142ZM13.28 7.22a.75.75 0 1 0-1.06 1.06L13.94 10l-1.72 1.72a.75.75 0 0 0 1.06 1.06L15 11.06l1.72 1.72a.75.75 0 1 0 1.06-1.06L16.06 10l1.72-1.72a.75.75 0 0 0-1.06-1.06L15 8.94l-1.72-1.72Z" />
            </svg>
          ) : (
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 20 20"
              fill="currentColor"
              className="h-4 w-4"
            >
              <path d="M10 3.75a.75.75 0 0 0-1.264-.546L5.203 6.5H2.667a.75.75 0 0 0-.7.48A6.985 6.985 0 0 0 1.5 10c0 .85.151 1.665.429 2.42a.75.75 0 0 0 .737.58h2.499l3.533 3.296A.75.75 0 0 0 10 15.75V3.75ZM15.95 5.05a.75.75 0 0 0-1.06 1.06 5.5 5.5 0 0 1 0 7.78.75.75 0 0 0 1.06 1.06 7 7 0 0 0 0-9.9Z" />
              <path d="M13.829 7.172a.75.75 0 0 0-1.06 1.06 2.5 2.5 0 0 1 0 3.536.75.75 0 0 0 1.06 1.06 4 4 0 0 0 0-5.656Z" />
            </svg>
          )}
        </button>
      </div>
      <div className="hidden sm:block">
        <PhaseIndicatorRight />
      </div>
      <div className="flex items-center justify-center gap-2 sm:hidden">
        <PhaseIndicatorLeft />
        <PhaseIndicatorRight />
      </div>
    </div>
  );
}
