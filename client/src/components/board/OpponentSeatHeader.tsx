import { useCallback, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { usePlayerDesignations } from "../../hooks/usePlayerDesignations.ts";
import { getSeatColor } from "../../hooks/useSeatColor.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName, useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { LifeTotal } from "../controls/LifeTotal.tsx";
import {
  CityBlessingBadge,
  ConditionBadge,
  CounterBadge,
  DungeonBadge,
  familyOf,
  InitiativeBadge,
  MonarchBadge,
  PendingSpellBadge,
  StatusBadge,
  UnboundedBadge,
} from "../hud/HudBadges.tsx";
import { AvatarHoverPreview } from "../hud/AvatarHoverPreview.tsx";
import { KickConfirmDialog } from "../hud/KickConfirmDialog.tsx";
import { NextUpBadge } from "../hud/NextUpBadge.tsx";

interface OpponentSeatHeaderProps {
  playerId: PlayerId;
  compact?: boolean;
  onKickPlayer?: (playerId: PlayerId) => void;
}

export function OpponentSeatHeader({ playerId, compact = false, onKickPlayer }: OpponentSeatHeaderProps) {
  const { t } = useTranslation("game");
  const myId = usePerspectivePlayerId();
  const [kickOpen, setKickOpen] = useState(false);
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const seatColor = getSeatColor(playerId, gameState?.seat_order);
  const avatarUrl = useMultiplayerStore((s) => s.playerAvatars.get(playerId) ?? null);
  const disconnected = useMultiplayerStore((s) => s.disconnectedPlayers.has(playerId));
  const isOnline = useMultiplayerStore((s) => s.connectionStatus) !== "disconnected";
  const designations = usePlayerDesignations(playerId);
  const player = gameState?.players[playerId];
  const label = getOpponentDisplayName(playerId);

  const currentLegalTargets = useMemo(() => {
    if (
      (waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection")
      && waitingFor.data.player === myId
    ) {
      return waitingFor.data.selection?.current_legal_targets ?? [];
    }
    if (waitingFor?.type === "CopyRetarget" && waitingFor.data.player === myId) {
      const slot = waitingFor.data.target_slots[waitingFor.data.current_slot ?? 0];
      return slot?.legal_alternatives ?? [];
    }
    if (
      waitingFor?.type === "RetargetChoice"
      && waitingFor.data.player === myId
      && waitingFor.data.scope.type === "Single"
    ) {
      return waitingFor.data.legal_new_targets;
    }
    if (waitingFor?.type === "ReturnAsAuraTarget" && waitingFor.data.player === myId) {
      return waitingFor.data.legal_targets;
    }
    return [];
  }, [myId, waitingFor]);

  const isValidPlayerTarget = currentLegalTargets.some(
    (target) => "Player" in target && target.Player === playerId,
  );
  const isTheirTurn = gameState?.active_player === playerId;
  const isUnderAttack = gameState?.combat?.attackers.some(
    (attacker) => attacker.attack_target.type === "Player" && attacker.attack_target.data === playerId,
  ) ?? false;
  const isPhasedOut = player?.status?.type === "PhasedOut";
  const poisonCounters = player?.poison_counters ?? 0;
  const radCounters = player?.player_counters?.Rad ?? 0;
  const experienceCounters = player?.player_counters?.Experience ?? 0;
  const speed = player?.speed ?? 0;

  const choosePlayerTarget = useCallback(() => {
    if (!isValidPlayerTarget) return;
    dispatch({ type: "ChooseTarget", data: { target: { Player: playerId } } });
  }, [dispatch, isValidPlayerTarget, playerId]);

  if (!player) return null;

  const rootChrome = compact
    ? "h-[var(--game-seat-header-height,1.85rem)] gap-1 px-1 py-0.5"
    : "h-[var(--game-seat-header-height,2.25rem)] gap-1.5 px-1.5 py-1";
  const avatarSize = compact ? "h-6 w-6 text-[10px]" : "h-7 w-7 text-[11px]";
  const labelWidth = compact ? "max-w-[5.75rem]" : "max-w-[11rem]";
  const badgeScale = compact ? "[&>*]:scale-[0.62]" : "[&>*]:scale-75";
  const identityWidth = compact ? "w-[min(72%,18rem)]" : "w-[min(64%,28rem)]";
  const rootTargetChrome = isValidPlayerTarget
    ? "cursor-pointer hover:border-cyan-300/70 hover:bg-cyan-950/45 hover:shadow-[0_0_24px_rgba(34,211,238,0.22)]"
    : "";
  const activeTurnChrome = isTheirTurn
    ? "border-rose-300/70 bg-rose-950/58 shadow-[0_10px_26px_rgba(244,63,94,0.28)] after:absolute after:inset-x-1 after:bottom-0 after:h-0.5 after:rounded-full after:bg-rose-300 after:shadow-[0_0_10px_rgba(251,113,133,0.95)]"
    : "";

  return (
    <div
      className={`relative z-10 flex min-w-0 items-center justify-end rounded-sm border border-white/8 bg-slate-950/64 shadow-[0_8px_18px_rgba(0,0,0,0.24)] backdrop-blur-md transition-[background-color,border-color,box-shadow] duration-150 ${rootChrome} ${activeTurnChrome} ${rootTargetChrome} ${
        isValidPlayerTarget ? "ring-1 ring-cyan-300/55" : ""
      }`}
      data-testid={`opponent-seat-header-${playerId}`}
      data-player-hud={String(playerId)}
      style={{ borderTopColor: `${seatColor}aa` }}
    >
      <NextUpBadge
        playerId={playerId}
        compact={compact}
        className="absolute left-1/2 top-0.5 z-30 -translate-x-1/2 -translate-y-1/2"
      />
      {isValidPlayerTarget ? (
        <button
          type="button"
          className="absolute inset-0 z-0 cursor-pointer rounded-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-cyan-200/75 focus-visible:ring-offset-1 focus-visible:ring-offset-slate-950"
          onClick={choosePlayerTarget}
          aria-label={t("opponentHud.targetPlayer", { name: label })}
          title={t("opponentHud.clickToTarget", { name: label })}
        />
      ) : null}
      <div className={`pointer-events-none absolute left-1/2 top-1/2 z-10 flex min-w-0 ${identityWidth} -translate-x-1/2 -translate-y-1/2 items-center justify-center ${compact ? "gap-1" : "gap-1.5"}`}>
        {avatarUrl ? (
          // Portrait-preview on hover, matching the 1v1 OpponentHud avatar. The
          // enclosing identity block is pointer-events-none, so the tile must
          // re-enable pointer events to receive hover — except while this seat is
          // a legal target, where the header's full-area target button must win.
          <AvatarHoverPreview
            avatarUrl={avatarUrl}
            label={label}
            seatColor={seatColor}
            title={isValidPlayerTarget ? t("opponentHud.clickToTarget", { name: label }) : label}
            className={`relative flex shrink-0 items-center justify-center overflow-hidden rounded-md border bg-slate-950 font-bold text-white transition ${avatarSize} ${
              isValidPlayerTarget ? "pointer-events-none ring-2 ring-cyan-300/70" : "pointer-events-auto"
            }`}
            style={{ borderColor: `${seatColor}cc`, backgroundColor: `${seatColor}44` }}
          >
            <img src={avatarUrl} alt={label} className="h-full w-full object-cover" />
            {isUnderAttack && <span className="absolute inset-0 rounded-md ring-2 ring-red-400/70" />}
          </AvatarHoverPreview>
        ) : (
          <div
            className={`relative flex shrink-0 items-center justify-center overflow-hidden rounded-md border bg-slate-950 font-bold text-white transition ${avatarSize} ${
              isValidPlayerTarget ? "ring-2 ring-cyan-300/70" : ""
            }`}
            style={{ borderColor: `${seatColor}cc`, backgroundColor: `${seatColor}44` }}
            title={label}
          >
            {label.charAt(0).toUpperCase()}
            {isUnderAttack && <span className="absolute inset-0 rounded-md ring-2 ring-red-400/70" />}
          </div>
        )}

        <div className={`flex min-w-0 shrink items-center justify-center ${compact ? "gap-1" : "gap-1.5"}`}>
          <span
            className={`min-w-0 truncate text-center text-[10px] font-bold uppercase tracking-[0.16em] ${labelWidth}`}
            style={{ color: seatColor }}
          >
            {label}
          </span>
          {isOnline && (
            <span
              className={`h-1.5 w-1.5 shrink-0 rounded-full ${disconnected ? "bg-red-500" : "bg-emerald-400"}`}
              title={disconnected ? t("opponentHud.disconnected") : t("opponentHud.connected")}
            />
          )}
          <LifeTotal playerId={playerId} size="sm" hideLabel />
          <div className={`flex min-w-0 max-w-[9rem] shrink items-center justify-end gap-0.5 overflow-hidden ${badgeScale} [&>*]:origin-right`}>
            {designations.isMonarch ? <MonarchBadge /> : null}
            {designations.hasInitiative ? <InitiativeBadge /> : null}
            {designations.hasCityBlessing ? <CityBlessingBadge /> : null}
            {designations.activeDungeon ? (
              <DungeonBadge
                dungeonName={designations.activeDungeon}
                roomIndex={designations.currentRoom}
              />
            ) : null}
            {designations.ringLevel > 0 ? (
              <CounterBadge
                kind="ring"
                value={designations.ringLevel}
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
            {[...new Set(designations.unboundedResources.map((resource) => familyOf(resource.axis)))].map(
              (family) => <UnboundedBadge key={family} family={family} />,
            )}
            {isPhasedOut ? <StatusBadge label={t("player.phasedOut")} tone="neutral" /> : null}
            {player.companion ? <StatusBadge label={t("badges.companion")} /> : null}
          </div>
        </div>
      </div>

      {onKickPlayer && (
        <button
          type="button"
          className="relative z-20 flex h-5 w-5 shrink-0 cursor-pointer items-center justify-center rounded-full bg-red-950/70 text-[12px] font-bold text-red-200 ring-1 ring-red-400/30 hover:bg-red-800"
          aria-label={t("opponentHud.kickPlayer", { seat: playerId + 1 })}
          title={t("opponentHud.kickPlayerTooltip")}
          onClick={(event) => {
            event.stopPropagation();
            setKickOpen(true);
          }}
        >
          ×
        </button>
      )}

      {createPortal(
        <KickConfirmDialog
          isOpen={kickOpen}
          playerLabel={label}
          onConfirm={() => {
            onKickPlayer?.(playerId);
            setKickOpen(false);
          }}
          onCancel={() => setKickOpen(false)}
        />,
        document.body,
      )}
    </div>
  );
}
