import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import { useCanActForWaitingState, usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { usePlayerDesignations } from "../../hooks/usePlayerDesignations.ts";
import { useSeatColor } from "../../hooks/useSeatColor.ts";
import { useTurnStatus } from "../../hooks/useTurnStatus.ts";
import { isAuthorityRemote, useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName, useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { getWaitingForPlayerChoiceIds } from "../../viewmodel/gameStateView.ts";
import { ScoreBadge } from "../draft/ScoreBadge.tsx";
import { ManualManaToggle } from "../board/ManualManaToggle.tsx";
import { UndoButton } from "../board/UndoButton.tsx";
import { FullControlToggle } from "../controls/FullControlToggle.tsx";
import { LifeTotal } from "../controls/LifeTotal.tsx";
import { MajorPhaseStopRail } from "../controls/PhaseStopBar.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";
import { CityBlessingBadge, ConditionBadge, CounterBadge, DungeonBadge, EnduringStoryBadge, InitiativeBadge, MonarchBadge, PendingSpellBadge, RingBenefitsBadge, StatusBadge, UnboundedBadge } from "./HudBadges.tsx";
import { EnchantmentsBadge } from "./EnchantmentsBadge.tsx";
import { HudPlate } from "./HudPlate.tsx";
import { NextUpBadge } from "./NextUpBadge.tsx";
import { PriorityMarker } from "./TurnStatusLine.tsx";
import { StormCounter } from "./StormCounter.tsx";
import { PlayerLatency } from "./PlayerLatency.tsx";

interface PlayerHudProps {
  /** Hide the redundant local-player label when TabletopGameBoard pins the
   *  portrait-filled life pill to the exact bottom-center edge. */
  alignNameplateToAnchor?: boolean;
}

export function PlayerHud({ alignNameplateToAnchor = false }: PlayerHudProps = {}) {
  const { t } = useTranslation("game");
  const playerId = usePerspectivePlayerId();
  const isMyTurn = useGameStore((s) => s.gameState?.active_player === playerId);
  const speed = useGameStore((s) => s.gameState?.players[playerId]?.speed ?? 0);
  const poisonCounters = useGameStore((s) => s.gameState?.players[playerId]?.poison_counters ?? 0);
  const radCounters = useGameStore((s) => s.gameState?.players[playerId]?.player_counters?.Rad ?? 0);
  const experienceCounters = useGameStore((s) => s.gameState?.players[playerId]?.player_counters?.Experience ?? 0);
  const designations = usePlayerDesignations(playerId);
  const isPhasedOut = useGameStore(
    (s) => s.gameState?.players[playerId]?.status?.type === "PhasedOut",
  );
  const isUnderAttack = useGameStore(
    (s) => s.gameState?.combat?.attackers.some(
      (a) => a.attack_target.type === "Player" && a.attack_target.data === playerId,
    ) ?? false,
  );
  const matchScore = useGameStore((s) => s.gameState?.match_score ?? null);
  const showMatchScore = useGameStore((s) => s.gameState?.match_config?.match_type === "Bo3");
  const stormCount = useGameStore((s) => s.gameState?.derived?.storm_count ?? 0);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const canUndo = useGameStore(
    (s) => s.stateHistory.length > 0 && !isAuthorityRemote(s.gameMode),
  );
  const { waitingSeatId, reason } = useTurnStatus();

  const canActForWaitingState = useCanActForWaitingState();
  // CR 115.1: the engine's legal set can name this seat. `getWaitingForPlayerChoiceIds`
  // is the single WaitingFor -> choosable-PlayerId authority every seat-rendering
  // surface reads, so a new player-targetable prompt lights this HUD up without a
  // per-variant branch here. The hook resolves the REAL seat (may this client
  // answer?); `playerId` is the RENDERED seat (did the engine name it?). CR 723:
  // under a turn-control effect these are different players, and both questions
  // still have to be answered.
  const isValidTarget =
    canActForWaitingState && getWaitingForPlayerChoiceIds(waitingFor).includes(playerId);

  const handleTargetClick = useCallback(() => {
    if (isValidTarget) {
      dispatch({ type: "ChooseTarget", data: { target: { Player: playerId } } });
    }
  }, [isValidTarget, dispatch, playerId]);

  const hudTone = isValidTarget ? "cyan" : isMyTurn ? "emerald" : "neutral";
  const seatColor = useSeatColor(playerId);
  const avatarIdentity = useMultiplayerStore((s) => s.playerAvatars.get(playerId) ?? null);
  const priorityTitle = waitingSeatId === playerId
    ? t(reason?.key ?? "status.reason.thinking", reason?.params)
    : undefined;
  const cornerBadges = (
    <div className="flex items-center gap-1">
      <NextUpBadge playerId={playerId} compact />
      <PriorityMarker
        active={waitingSeatId === playerId}
        reasonKey={reason?.key}
        seatColor={seatColor}
        title={priorityTitle}
      />
    </div>
  );
  const statusBadges = (
    <>
      <ManaPoolSummary playerId={playerId} size="sm" />
      <EnchantmentsBadge playerId={playerId} />
      <StormCounter count={stormCount} />
      {showMatchScore && matchScore ? <ScoreBadge score={matchScore} player={0} /> : null}
      {designations.isMonarch ? <MonarchBadge /> : null}
      {designations.hasInitiative ? <InitiativeBadge /> : null}
      {designations.hasCityBlessing ? <CityBlessingBadge /> : null}
      {designations.hasEnduringStory ? <EnduringStoryBadge /> : null}
      {designations.dungeonRoom ? <DungeonBadge room={designations.dungeonRoom} /> : null}
      {isPhasedOut ? <StatusBadge label={t("player.phasedOut")} tone="neutral" /> : null}
      {designations.ringLevel > 0 ? (
        <RingBenefitsBadge
          level={designations.ringLevel}
          ringBearerName={designations.ringBearerName}
        />
      ) : null}
      {designations.energy > 0 ? <CounterBadge kind="energy" value={designations.energy} /> : null}
      {poisonCounters > 0 ? <CounterBadge kind="poison" value={poisonCounters} /> : null}
      {radCounters > 0 ? <CounterBadge kind="rad" value={radCounters} /> : null}
      {experienceCounters > 0 ? <CounterBadge kind="experience" value={experienceCounters} /> : null}
      {speed > 0 ? <CounterBadge kind="speed" value={speed} /> : null}
      {designations.pendingSpellModifiers.length > 0
      || designations.pendingSpellReductions.length > 0 ? (
        <PendingSpellBadge
          modifiers={designations.pendingSpellModifiers}
          reductions={designations.pendingSpellReductions}
        />
      ) : null}
      {designations.statusConditions.map((condition, i) => (
        <ConditionBadge
          key={`${condition.kind.type}-${condition.source ?? "x"}-${i}`}
          condition={condition}
        />
      ))}
      {designations.unboundedFamilies.map((u) => (
        <UnboundedBadge key={u.family} family={u.family} state={u.state} />
      ))}
    </>
  );

  return (
    <div
      data-player-hud={playerId}
      data-local-player-hud=""
      data-edge-pill-layout="true"
      data-player-life-shape="pill"
      data-phased-out={isPhasedOut ? "true" : undefined}
      className={`relative z-20 flex shrink-0 flex-row flex-nowrap items-center justify-center gap-0 p-0 ${
        isPhasedOut ? "opacity-40 grayscale" : ""
      }`}
    >
      <div
        className="pointer-events-auto absolute left-1/2 z-[1] flex -translate-x-1/2 items-center"
        data-player-hud-phase-stop-rail=""
      >
        <MajorPhaseStopRail />
      </div>
      <HudPlate
        label={getPlayerDisplayName(playerId, playerId)}
        hideLabel={alignNameplateToAnchor}
        tone={hudTone}
        active={isMyTurn}
        seatColor={seatColor}
        underAttack={isUnderAttack}
        avatarIdentity={avatarIdentity}
        playerId={playerId}
        density="compact"
        onClick={isValidTarget ? handleTargetClick : undefined}
      >
        <div className="flex min-w-0 items-center gap-1">
          <LifeTotal playerId={playerId} size="lg" hideLabel />
          <PlayerLatency playerId={playerId} />
        </div>
      </HudPlate>
      <div
        className="pointer-events-none absolute right-0 top-0 z-20 flex max-w-[calc(100vw-1rem)] translate-x-1/2 -translate-y-1/2 items-center justify-center gap-1 [&>*]:pointer-events-auto"
        data-player-hud-edge-statuses=""
      >
        {cornerBadges}
        {statusBadges}
      </div>
      <div
        className="pointer-events-auto fixed z-20 flex flex-col gap-2"
        data-player-hud-corner-controls=""
      >
        <FullControlToggle iconOnly />
        <ManualManaToggle iconOnly />
      </div>
      {canUndo ? (
        <div
          className="pointer-events-auto fixed z-40"
          data-player-hud-undo-control=""
        >
          <UndoButton iconOnly />
        </div>
      ) : null}
    </div>
  );
}
