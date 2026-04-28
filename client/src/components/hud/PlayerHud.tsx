import { useCallback } from "react";

import { usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { useSeatColor } from "../../hooks/useSeatColor.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";
import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";
import { PhaseIndicatorLeft, PhaseIndicatorRight } from "../controls/PhaseStopBar.tsx";
import { CounterBadge, StatusBadge } from "./HudBadges.tsx";
import { HudPlate } from "./HudPlate.tsx";

export function PlayerHud() {
  const playerId = usePerspectivePlayerId();
  const isMyTurn = useGameStore((s) => s.gameState?.active_player === playerId);
  const speed = useGameStore((s) => s.gameState?.players[playerId]?.speed ?? 0);
  const poisonCounters = useGameStore((s) => s.gameState?.players[playerId]?.poison_counters ?? 0);
  const isPhasedOut = useGameStore(
    (s) => s.gameState?.players[playerId]?.status?.type === "PhasedOut",
  );
  const isUnderAttack = useGameStore(
    (s) => s.gameState?.combat?.attackers.some(
      (a) => a.attack_target.type === "Player" && a.attack_target.data === playerId,
    ) ?? false,
  );
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

  const hudTone = isValidTarget ? "cyan" : isMyTurn ? "emerald" : "neutral";
  const seatColor = useSeatColor(playerId);

  return (
    <div
      data-player-hud={playerId}
      data-phased-out={isPhasedOut ? "true" : undefined}
      className={`relative z-20 flex shrink-0 flex-row flex-nowrap items-center justify-center gap-1.5 px-1 py-1 lg:gap-2 lg:px-2 ${
        isPhasedOut ? "opacity-40 grayscale" : ""
      }`}
    >
      <PhaseIndicatorLeft />
      <HudPlate
        label={getPlayerDisplayName(playerId)}
        tone={hudTone}
        active={isMyTurn}
        seatColor={seatColor}
        underAttack={isUnderAttack}
        onClick={isValidTarget ? handleTargetClick : undefined}
        trailing={
          <>
            {isPhasedOut ? <StatusBadge label="Phased Out" tone="neutral" /> : null}
            {poisonCounters > 0 ? <CounterBadge kind="poison" value={poisonCounters} /> : null}
            {speed > 0 ? <CounterBadge kind="speed" value={speed} /> : null}
          </>
        }
      >
        <div className="flex min-w-0 items-center gap-2">
          <LifeTotal playerId={playerId} size="lg" hideLabel />
          <ManaPoolSummary playerId={playerId} />
        </div>
      </HudPlate>
      <PhaseIndicatorRight />
    </div>
  );
}
