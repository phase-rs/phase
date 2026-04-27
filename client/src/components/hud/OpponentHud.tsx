import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { motion, useReducedMotion } from "framer-motion";

import type { PlayerId } from "../../adapter/types.ts";
import { usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { getSeatColor } from "../../hooks/useSeatColor.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName, useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { partitionByType } from "../../viewmodel/battlefieldProps.ts";
import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";
import { CounterBadge, StatusBadge } from "./HudBadges.tsx";
import { HudPlate } from "./HudPlate.tsx";
import { IncomingAttackersPopover } from "./IncomingAttackersPopover.tsx";
import { KickConfirmDialog } from "./KickConfirmDialog.tsx";
import { UnderAttackOverlay } from "./UnderAttackOverlay.tsx";

import type { ObjectId } from "../../adapter/types.ts";

const EMPTY_ATTACKER_IDS: readonly ObjectId[] = [];

interface OpponentHudProps {
  opponentName?: string | null;
  /**
   * P2P host-only callback to kick a player. When provided AND the game is
   * 3+ players, an inline kick button appears on each opponent tab. The
   * adapter handles auto-concede + denylist + wire broadcast.
   */
  onKickPlayer?: (playerId: PlayerId) => void;
}

export function OpponentHud({ opponentName, onKickPlayer }: OpponentHudProps) {
  const [kickTarget, setKickTarget] = useState<PlayerId | null>(null);
  const playerId = usePerspectivePlayerId();
  const focusedOpponent = useUiStore((s) => s.focusedOpponent) as PlayerId | null;
  const setFocusedOpponent = useUiStore((s) => s.setFocusedOpponent);
  const followActiveOpponent = usePreferencesStore((s) => s.followActiveOpponent);
  const setFollowActiveOpponent = usePreferencesStore((s) => s.setFollowActiveOpponent);
  const gameState = useGameStore((s) => s.gameState);

  const teamBased = gameState?.format_config?.team_based ?? false;

  const allOpponents = useMemo(() => {
    if (!gameState) return [];
    const seatOrder = gameState.seat_order ?? gameState.players.map((p) => p.id);
    return seatOrder.filter((id) => id !== playerId);
  }, [gameState, playerId]);

  const eliminated = gameState?.eliminated_players ?? [];
  const liveOpponents = allOpponents.filter((id) => !eliminated.includes(id));
  const isMultiplayer = allOpponents.length > 1;

  // The `OpponentTab` row renders with a default-focused opponent even when
  // `focusedOpponent` is null (it falls back to the first live opponent).
  // The cross-board glimpse must exclude the *visually* focused opponent,
  // not just the explicit one — otherwise the default-focused tab lights
  // up a redundant badge at game start.
  const effectiveFocused = focusedOpponent ?? liveOpponents[0] ?? null;

  // Cross-board attacker glimpse: for each non-focused opponent, collect the
  // ids of their creatures currently attacking the local player or their
  // permanents. Used by `OpponentTab` to render a badge + hover popover so
  // the defender can assess incoming threats without switching focus.
  const attackers = gameState?.combat?.attackers;
  const objectsMap = gameState?.objects;
  const incomingByOpponent = useMemo(() => {
    const map = new Map<PlayerId, ObjectId[]>();
    if (!attackers || !objectsMap) return map;
    for (const attacker of attackers) {
      const attackerObj = objectsMap[attacker.object_id];
      if (!attackerObj) continue;
      const controller = attackerObj.controller;
      // Skip my own attackers; they can't be attacking me.
      if (controller === playerId) continue;
      // Skip the focused opponent — their board is on screen, arrows already
      // draw. The badge would be redundant.
      if (effectiveFocused != null && controller === effectiveFocused) continue;

      const t = attacker.attack_target;
      const targetsMe =
        (t.type === "Player" && t.data === playerId)
        || ((t.type === "Planeswalker" || t.type === "Battle")
          && objectsMap[t.data]?.controller === playerId);
      if (!targetsMe) continue;

      const list = map.get(controller) ?? [];
      list.push(attacker.object_id);
      map.set(controller, list);
    }
    return map;
  }, [attackers, objectsMap, playerId, effectiveFocused]);

  useEffect(() => {
    const activeOpponentId = gameState?.active_player;
    if (!followActiveOpponent || !isMultiplayer || activeOpponentId == null) {
      return;
    }
    if (!liveOpponents.includes(activeOpponentId) || focusedOpponent === activeOpponentId) {
      return;
    }
    setFocusedOpponent(activeOpponentId);
  }, [
    followActiveOpponent,
    focusedOpponent,
    gameState?.active_player,
    isMultiplayer,
    liveOpponents,
    setFocusedOpponent,
  ]);

  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const isHumanTargetSelection =
    (waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection")
    && waitingFor.data.player === playerId;
  const validPlayerTargetIds = useMemo(() => {
    if (!isHumanTargetSelection) return [] as number[];
    return (waitingFor.data.selection?.current_legal_targets ?? [])
      .filter((target): target is { Player: number } => "Player" in target)
      .map((target) => target.Player);
  }, [isHumanTargetSelection, waitingFor]);

  const handlePlayerTarget = useCallback(
    (targetPlayerId: number) => {
      dispatch({ type: "ChooseTarget", data: { target: { Player: targetPlayerId } } });
    },
    [dispatch],
  );

  const disconnectedPlayers = useMultiplayerStore((s) => s.disconnectedPlayers);
  const connectionStatus = useMultiplayerStore((s) => s.connectionStatus);
  const isOnline = connectionStatus !== "disconnected";

  if (!isMultiplayer) {
    // 1v1: single opponent pill (existing design)
    const opponentId = allOpponents[0] ?? (playerId === 0 ? 1 : 0);
    const isOpponentTurn = gameState?.active_player === opponentId;
    const isValidTarget = validPlayerTargetIds.includes(opponentId);
    const opponentCompanion = gameState?.players[opponentId]?.companion;
    const opponentSpeed = gameState?.players[opponentId]?.speed ?? 0;
    const opponentPoisonCounters = gameState?.players[opponentId]?.poison_counters ?? 0;
    const isDisconnected = isOnline && disconnectedPlayers.has(opponentId);
    const isOpponentPhasedOut =
      gameState?.players[opponentId]?.status?.type === "PhasedOut";
    const label = opponentName ?? getOpponentDisplayName(opponentId);

    const hudTone = isValidTarget ? "cyan" : isOpponentTurn ? "rose" : "neutral";
    const opponentSeatColor = getSeatColor(opponentId, gameState?.seat_order);
    const isOpponentUnderAttack = gameState?.combat?.attackers.some(
      (a) => a.attack_target.type === "Player" && a.attack_target.data === opponentId,
    ) ?? false;

    return (
      <div
        data-player-hud={String(opponentId)}
        data-phased-out={isOpponentPhasedOut ? "true" : undefined}
        className={`relative flex items-center py-1 ${
          isOpponentPhasedOut ? "opacity-40 grayscale" : ""
        }`}
      >
        <HudPlate
          label={label}
          tone={hudTone}
          active={isOpponentTurn}
          seatColor={opponentSeatColor}
          underAttack={isOpponentUnderAttack}
          onClick={isValidTarget ? () => handlePlayerTarget(opponentId) : undefined}
          trailing={opponentPoisonCounters > 0 || opponentSpeed > 0 || opponentCompanion || isOnline || isOpponentPhasedOut ? (
            <>
              {isOpponentPhasedOut ? <StatusBadge label="Phased Out" tone="neutral" /> : null}
              {opponentPoisonCounters > 0 ? <CounterBadge kind="poison" value={opponentPoisonCounters} /> : null}
              {opponentSpeed > 0 ? <CounterBadge kind="speed" value={opponentSpeed} /> : null}
              {opponentCompanion ? <StatusBadge label="Companion" /> : null}
              {isOnline ? <ConnectionDotInline disconnected={isDisconnected} /> : null}
            </>
          ) : undefined}
        >
          <div className="flex min-w-0 items-center gap-2">
            <LifeTotal playerId={opponentId} size="lg" hideLabel />
            <ManaPoolSummary playerId={opponentId} />
          </div>
        </HudPlate>
      </div>
    );
  }

  // Multiplayer: tabbed opponent selector
  const focusedId = focusedOpponent ?? liveOpponents[0];
  const targetLabel = kickTarget != null ? getOpponentDisplayName(kickTarget) : "";

  return (
    <div className="flex items-center justify-center gap-2 px-2 py-1">
      {allOpponents.map((opId) => (
        <OpponentTab
          key={opId}
          playerId={opId}
          isFocused={focusedId === opId}
          isEliminated={eliminated.includes(opId)}
          isTeammate={teamBased && isTeammate(playerId, opId)}
          isValidTarget={validPlayerTargetIds.includes(opId)}
          showMana={focusedId === opId}
          incomingAttackerIds={incomingByOpponent.get(opId) ?? EMPTY_ATTACKER_IDS}
          onClick={() => validPlayerTargetIds.includes(opId) ? handlePlayerTarget(opId) : setFocusedOpponent(opId)}
          onKick={
            onKickPlayer && !eliminated.includes(opId)
              ? () => setKickTarget(opId)
              : undefined
          }
        />
      ))}
      <KickConfirmDialog
        isOpen={kickTarget !== null}
        playerLabel={targetLabel}
        onConfirm={() => {
          if (kickTarget !== null && onKickPlayer) onKickPlayer(kickTarget);
          setKickTarget(null);
        }}
        onCancel={() => setKickTarget(null)}
      />
      <button
        type="button"
        aria-pressed={followActiveOpponent}
        onClick={() => setFollowActiveOpponent(!followActiveOpponent)}
        className={`rounded-full border px-3 py-1.5 text-[11px] font-semibold uppercase tracking-[0.16em] backdrop-blur-xl transition-all duration-200 ${
          followActiveOpponent
            ? "border-amber-300/35 bg-amber-500/18 text-amber-100 shadow-[0_10px_24px_rgba(245,158,11,0.18)]"
            : "border-white/10 bg-slate-950/62 text-slate-300 hover:border-white/20 hover:text-white"
        }`}
      >
        Follow
      </button>
    </div>
  );
}

/** 2HG team pairing: players 0+1 are team A, 2+3 are team B. */
function isTeammate(a: PlayerId, b: PlayerId): boolean {
  return Math.floor(a / 2) === Math.floor(b / 2);
}

interface OpponentTabProps {
  playerId: PlayerId;
  isFocused: boolean;
  isEliminated: boolean;
  isTeammate: boolean;
  isValidTarget: boolean;
  showMana: boolean;
  /** Attacker object ids this opponent has declared against me / my stuff.
   *  When non-empty, the tab renders a red ⚔×N badge and a hover popover
   *  with mini card images so the defender can assess incoming threats
   *  without first focusing this opponent's board. */
  incomingAttackerIds: readonly ObjectId[];
  onClick: () => void;
  /** Host-only: when provided, render a small kick affordance on the tab. */
  onKick?: () => void;
}

function OpponentTab({ playerId, isFocused, isEliminated, isTeammate: ally, isValidTarget, showMana, incomingAttackerIds, onClick, onKick }: OpponentTabProps) {
  const gameState = useGameStore((s) => s.gameState);
  const isTheirTurn = gameState?.active_player === playerId;
  const seatColor = getSeatColor(playerId, gameState?.seat_order);
  const isUnderAttack = gameState?.combat?.attackers.some(
    (a) => a.attack_target.type === "Player" && a.attack_target.data === playerId,
  ) ?? false;
  const [showIncomingPopover, setShowIncomingPopover] = useState(false);
  const hasIncoming = incomingAttackerIds.length > 0;
  // Short close delay so cursor moving through the gap between the tab and
  // the popover below doesn't flicker the popover shut. The popover itself
  // is `pointer-events-none`, so it can't re-enter the button — the delay
  // is the only UX-safe way to give the reader time to parse mini cards.
  const closeTimerRef = useRef<number | null>(null);
  const openPopover = useCallback(() => {
    if (closeTimerRef.current != null) {
      window.clearTimeout(closeTimerRef.current);
      closeTimerRef.current = null;
    }
    setShowIncomingPopover(true);
  }, []);
  const scheduleClosePopover = useCallback(() => {
    if (closeTimerRef.current != null) window.clearTimeout(closeTimerRef.current);
    closeTimerRef.current = window.setTimeout(() => {
      setShowIncomingPopover(false);
      closeTimerRef.current = null;
    }, 180);
  }, []);
  useEffect(() => () => {
    if (closeTimerRef.current != null) window.clearTimeout(closeTimerRef.current);
  }, []);
  const player = gameState?.players[playerId];
  const isDisconnected = useMultiplayerStore((s) => s.disconnectedPlayers.has(playerId));
  const isOnline = useMultiplayerStore((s) => s.connectionStatus) !== "disconnected";
  const shouldReduceMotion = useReducedMotion();

  const counts = useMemo(() => {
    if (!gameState) return { creatures: 0, lands: 0, other: 0 };
    const objects = gameState.battlefield
      .map((id) => gameState.objects[id])
      .filter(Boolean)
      .filter((obj) => obj.controller === playerId);
    const partition = partitionByType(objects);
    return {
      creatures: partition.creatures.length,
      lands: partition.lands.length,
      other: partition.support.length + partition.planeswalkers.length + partition.other.length,
    };
  }, [gameState, playerId]);

  if (!player) return null;

  const handCount = player.hand.length;
  const speed = player.speed ?? 0;
  const poisonCounters = player.poison_counters;
  const isPhasedOut = player.status?.type === "PhasedOut";

  const label = ally ? "Ally" : getOpponentDisplayName(playerId);

  const borderClass = isValidTarget
    ? "border-cyan-400/45 bg-cyan-950/45 ring-1 ring-cyan-300/45 shadow-[0_14px_28px_rgba(34,211,238,0.16)] cursor-pointer"
    : isTheirTurn
      ? "border-rose-400/45 bg-rose-950/40 ring-2 ring-rose-300/70 ring-offset-2 ring-offset-black/40 shadow-[0_14px_28px_rgba(244,63,94,0.22)]"
      : ally
        ? isFocused
          ? "border-emerald-400/40 bg-emerald-950/40 ring-1 ring-emerald-300/30"
          : "border-emerald-700/40 bg-slate-950/70 hover:border-emerald-400/40 hover:bg-slate-900/72"
        : isFocused
          ? "border-amber-400/40 bg-amber-950/38 ring-1 ring-amber-300/30"
          : "border-white/10 bg-slate-950/70 hover:border-white/20 hover:bg-slate-900/72";

  return (
    <button
      type="button"
      onClick={onClick}
      disabled={isEliminated}
      data-player-hud={String(playerId)}
      data-phased-out={isPhasedOut ? "true" : undefined}
      onMouseEnter={hasIncoming ? openPopover : undefined}
      onMouseLeave={hasIncoming ? scheduleClosePopover : undefined}
      onFocus={hasIncoming ? openPopover : undefined}
      onBlur={hasIncoming ? scheduleClosePopover : undefined}
      className={`relative flex items-center gap-3 rounded-[18px] border px-3 py-2 backdrop-blur-xl transition-all duration-200 ${borderClass} ${isEliminated || isPhasedOut ? "opacity-40 grayscale" : ""}`}
    >
      {isTheirTurn && !shouldReduceMotion && (
        <motion.div
          aria-hidden
          className="pointer-events-none absolute -inset-0.5 rounded-[20px]"
          animate={{
            boxShadow: [
              "0 0 0 0 rgba(251, 113, 133, 0.35), 0 0 14px 2px rgba(251, 113, 133, 0.35)",
              "0 0 0 2px rgba(251, 113, 133, 0.65), 0 0 26px 6px rgba(251, 113, 133, 0.65)",
            ],
          }}
          transition={{
            duration: 1.2,
            repeat: Infinity,
            repeatType: "reverse",
            ease: "easeInOut",
          }}
        />
      )}
      {isUnderAttack && (
        <>
          <UnderAttackOverlay />
          <span className="sr-only">{label} is under attack</span>
        </>
      )}
      <div className="flex min-w-[4.5rem] flex-col items-start leading-none">
        <span className="mb-1 flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-[0.18em] text-white/48">
          <span
            aria-hidden
            className="h-1.5 w-1.5 shrink-0 rounded-full ring-1 ring-black/30"
            style={{ backgroundColor: seatColor }}
          />
          {label}
        </span>
        <div className="flex items-center gap-1">
          {isTheirTurn && <span className="h-1.5 w-1.5 rounded-full bg-rose-400 animate-pulse" />}
          <span className={`text-sm font-semibold ${isTheirTurn ? "text-rose-200" : ally ? "text-emerald-200" : isFocused ? "text-amber-100" : "text-slate-100"}`}>
            {player.life}
          </span>
          {poisonCounters > 0 ? <CounterBadge kind="poison" value={poisonCounters} /> : null}
          {speed > 0 ? <CounterBadge kind="speed" value={speed} /> : null}
          {isOnline && <ConnectionDotInline disconnected={isDisconnected} />}
        </div>
      </div>

      <Stat label="Hand" value={handCount} color="text-slate-200" />
      {counts.creatures > 0 && <Stat label="Creatures" value={counts.creatures} color="text-rose-200" />}
      {counts.lands > 0 && <Stat label="Lands" value={counts.lands} color="text-emerald-200" />}
      {counts.other > 0 && <Stat label="Other" value={counts.other} color="text-cyan-200" />}

      {player.companion != null && (
        <StatusBadge label="Companion" tone={player.companion.used ? "neutral" : "amber"} />
      )}

      {showMana && <ManaPoolSummary playerId={playerId} />}

      {isEliminated && (
        <span className="rounded-full bg-red-900/60 px-2 py-1 text-[10px] font-bold uppercase tracking-[0.16em] text-red-300">Out</span>
      )}

      {isPhasedOut && !isEliminated && (
        <span className="rounded-full bg-indigo-900/60 px-2 py-1 text-[10px] font-bold uppercase tracking-[0.16em] text-indigo-200">Phased</span>
      )}

      {onKick && !isEliminated && (
        // Stop propagation so clicking the kick affordance doesn't also fire
        // the parent button's `onClick` (which sets focused opponent or
        // selects a target).
        <span
          role="button"
          tabIndex={0}
          aria-label={`Kick player ${playerId + 1}`}
          onClick={(e) => {
            e.stopPropagation();
            onKick();
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.stopPropagation();
              e.preventDefault();
              onKick();
            }
          }}
          className="ml-1 flex h-5 w-5 cursor-pointer items-center justify-center rounded-full bg-red-900/40 text-[11px] font-bold text-red-300 ring-1 ring-red-500/30 transition hover:bg-red-700/60 hover:text-red-100"
          title="Kick player (forfeit)"
        >
          ×
        </span>
      )}
      {/* Cross-board attacker badge + hover popover — only when this
          non-focused opponent has declared attackers against me/my stuff.
          Left-positioned to avoid colliding with the right-edge kick `×`
          affordance rendered above. */}
      {hasIncoming && (
        <>
          <span
            aria-label={`${incomingAttackerIds.length} creature${incomingAttackerIds.length === 1 ? "" : "s"} attacking you`}
            className={`absolute -left-1.5 -top-1.5 z-10 flex h-5 min-w-5 items-center justify-center rounded-full bg-red-600 px-1 text-[10px] font-bold text-white shadow ring-2 ring-red-300 ${shouldReduceMotion ? "" : "animate-pulse"}`}
          >
            ⚔×{incomingAttackerIds.length}
          </span>
          {showIncomingPopover && (
            <IncomingAttackersPopover
              attackerIds={incomingAttackerIds}
              opponentName={label}
            />
          )}
        </>
      )}
    </button>
  );
}

function ConnectionDotInline({ disconnected }: { disconnected: boolean }) {
  return (
    <span
      className={`inline-block h-2 w-2 rounded-full ring-1 ring-white/20 ${disconnected ? "bg-red-500 animate-pulse" : "bg-emerald-400"}`}
      title={disconnected ? "Disconnected" : "Connected"}
    />
  );
}

function Stat({ label, value, color }: { label: string; value: number; color: string }) {
  return (
    <div className="flex flex-col items-start leading-none">
      <span className="mb-1 text-[9px] font-medium uppercase tracking-[0.16em] text-white/40">{label}</span>
      <span className={`text-sm font-semibold tabular-nums ${color}`}>{value}</span>
    </div>
  );
}
