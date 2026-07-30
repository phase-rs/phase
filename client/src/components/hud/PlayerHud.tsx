import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import { usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { usePlayerDesignations } from "../../hooks/usePlayerDesignations.ts";
import { useSeatColor } from "../../hooks/useSeatColor.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useIsMobile } from "../../hooks/useIsMobile.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName, useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { ScoreBadge } from "../draft/ScoreBadge.tsx";
import { ManualManaToggle } from "../board/ManualManaToggle.tsx";
import { UndoButton } from "../board/UndoButton.tsx";
import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";
import { PhaseIndicatorLeft, PhaseIndicatorRight } from "../controls/PhaseStopBar.tsx";
import { CityBlessingBadge, ConditionBadge, CounterBadge, DungeonBadge, familyOf, InitiativeBadge, MonarchBadge, PendingSpellBadge, RingBenefitsBadge, StatusBadge, UnboundedBadge } from "./HudBadges.tsx";
import { EnchantmentsBadge } from "./EnchantmentsBadge.tsx";
import { HudPlate } from "./HudPlate.tsx";
import { NextUpBadge } from "./NextUpBadge.tsx";

export function PlayerHud() {
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
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const isMobile = useIsMobile();
  const isCompactHeight = useIsCompactHeight();
  const compact = isMobile || isCompactHeight;

  const isHumanTargetSelection =
    (waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection")
    && waitingFor.data.player === playerId;
  const isCopyRetargetForMe = waitingFor?.type === "CopyRetarget" && waitingFor.data.player === playerId;
  const copyRetargetCurrentSlotHasMe = isCopyRetargetForMe && (() => {
    const slot = waitingFor.data.target_slots[waitingFor.data.current_slot ?? 0];
    return (slot?.legal_alternatives ?? []).some((t) => "Player" in t && t.Player === playerId);
  })();
  // CR 115.7: A single-target retarget (Bolt Bend on a spell aimed at a player)
  // can redirect to this player — same board-click path as normal targeting.
  const retargetChoiceHasMe = waitingFor?.type === "RetargetChoice"
    && waitingFor.data.player === playerId
    && waitingFor.data.scope.type === "Single"
    && waitingFor.data.legal_new_targets.some((t) => "Player" in t && t.Player === playerId);
  // CR 303.4g + CR 115.1: a returned / non-spell Aura that can enchant a player
  // (a Curse) is hosted by a board pick — the picker's controller may attach it
  // to this player when they appear in `legal_targets`. Click dispatches the
  // same `ChooseTarget { Player }` the engine accepts (engine.rs ~2984).
  const returnAsAuraHasMe = waitingFor?.type === "ReturnAsAuraTarget"
    && waitingFor.data.player === playerId
    && waitingFor.data.legal_targets.some((t) => "Player" in t && t.Player === playerId);
  const isValidTarget = (isHumanTargetSelection && (waitingFor.data.selection?.current_legal_targets ?? []).some(
    (target) => "Player" in target && target.Player === playerId,
  )) || copyRetargetCurrentSlotHasMe || retargetChoiceHasMe || returnAsAuraHasMe;

  const handleTargetClick = useCallback(() => {
    if (isValidTarget) {
      dispatch({ type: "ChooseTarget", data: { target: { Player: playerId } } });
    }
  }, [isValidTarget, dispatch, playerId]);

  const hudTone = isValidTarget ? "cyan" : isMyTurn ? "emerald" : "neutral";
  const seatColor = useSeatColor(playerId);
  const avatarUrl = useMultiplayerStore((s) => s.playerAvatars.get(playerId) ?? null);

  return (
    <div
      data-player-hud={playerId}
      data-phased-out={isPhasedOut ? "true" : undefined}
      className={`relative z-20 flex shrink-0 flex-row flex-nowrap items-center justify-center ${compact ? "gap-1 px-0.5 py-0.5" : "gap-1.5 px-1 py-1 lg:gap-2 lg:px-2"} ${
        isPhasedOut ? "opacity-40 grayscale" : ""
      }`}
    >
      <PhaseIndicatorLeft />
      <HudPlate
        label={getPlayerDisplayName(playerId, playerId)}
        tone={hudTone}
        active={isMyTurn}
        seatColor={seatColor}
        underAttack={isUnderAttack}
        avatarUrl={avatarUrl}
        playerId={playerId}
        density={compact ? "compact" : "default"}
        onClick={isValidTarget ? handleTargetClick : undefined}
        cornerBadge={<NextUpBadge playerId={playerId} compact={compact} />}
        trailing={
          <>
            <EnchantmentsBadge playerId={playerId} />
            {showMatchScore && matchScore ? <ScoreBadge score={matchScore} player={0} /> : null}
            {designations.isMonarch ? <MonarchBadge /> : null}
            {designations.hasInitiative ? <InitiativeBadge /> : null}
            {designations.hasCityBlessing ? <CityBlessingBadge /> : null}
            {designations.activeDungeon ? (
              <DungeonBadge dungeonName={designations.activeDungeon} roomIndex={designations.currentRoom} />
            ) : null}
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
            {[...new Set(designations.unboundedResources.map((u) => familyOf(u.axis)))].map(
              (family) => (
                <UnboundedBadge key={family} family={family} />
              ),
            )}
          </>
        }
      >
        <div className={`flex min-w-0 items-center ${compact ? "gap-1" : "gap-2"}`}>
          <LifeTotal playerId={playerId} size={compact ? "sm" : "lg"} hideLabel />
          <ManaPoolSummary playerId={playerId} size={compact ? "sm" : "default"} />
        </div>
      </HudPlate>
      <PhaseIndicatorRight />
      {/* Manual mana + undo ride the HUD (drag offsets and the mobile portrait
          shift included) instead of overlaying the land column, where they
          collided with land stacks and the zone piles. Absolutely positioned
          off the right edge so the plate keeps its centered anchor. The
          pointer-events split keeps the column's empty bounding-box regions
          (chip gap, short-chip gutter) tappable through to fanned hand cards. */}
      <div className="pointer-events-none absolute left-full top-1/2 z-20 ml-1 flex -translate-y-1/2 flex-col items-start gap-1 [&>*]:pointer-events-auto">
        <ManualManaToggle />
        <UndoButton />
      </div>
    </div>
  );
}
