import { type CSSProperties, useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { motion, useReducedMotion } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { usePlayerDesignations } from "../../hooks/usePlayerDesignations.ts";
import { getSeatColor } from "../../hooks/useSeatColor.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName, useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { partitionByType } from "../../viewmodel/battlefieldProps.ts";
import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";
import { ScoreBadge } from "../draft/ScoreBadge.tsx";
import { CityBlessingBadge, CounterBadge, DungeonBadge, InitiativeBadge, MonarchBadge, StatusBadge } from "./HudBadges.tsx";
import { AvatarHoverPreview } from "./AvatarHoverPreview.tsx";
import { BattlefieldPeekPopover } from "./BattlefieldPeekPopover.tsx";
import { EnchantmentsBadge } from "./EnchantmentsBadge.tsx";
import { HudPlate } from "./HudPlate.tsx";
import { IncomingAttackersPopover } from "./IncomingAttackersPopover.tsx";
import { KickConfirmDialog } from "./KickConfirmDialog.tsx";
import { UnderAttackOverlay } from "./UnderAttackOverlay.tsx";

import type { ObjectId } from "../../adapter/types.ts";

const EMPTY_OBJECT_IDS: readonly ObjectId[] = [];

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
  const { t } = useTranslation("game");
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
  const activeOpponentId = gameState?.active_player;
  const activeFollowedOpponent =
    activeOpponentId != null && liveOpponents.includes(activeOpponentId)
      ? activeOpponentId
      : null;

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

      const target = attacker.attack_target;
      const targetsMe =
        (target.type === "Player" && target.data === playerId)
        || ((target.type === "Planeswalker" || target.type === "Battle")
          && objectsMap[target.data]?.controller === playerId);
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
  const isCopyRetargetForMe = waitingFor?.type === "CopyRetarget" && waitingFor.data.player === playerId;
  const isTargeting = isHumanTargetSelection || isCopyRetargetForMe;
  const currentLegalTargets = useMemo(() => {
    if (isHumanTargetSelection) {
      return waitingFor.data.selection?.current_legal_targets ?? [];
    }
    if (isCopyRetargetForMe) {
      const slot = waitingFor.data.target_slots[waitingFor.data.current_slot ?? 0];
      return slot?.legal_alternatives ?? [];
    }
    return [];
  }, [isHumanTargetSelection, isCopyRetargetForMe, waitingFor]);
  const validPlayerTargetIds = useMemo(
    () => currentLegalTargets
      .filter((tgt): tgt is { Player: number } => "Player" in tgt)
      .map((tgt) => tgt.Player),
    [currentLegalTargets],
  );
  // Object targets grouped by their controller, so each `OpponentTab` can
  // show only that opponent's legal targets in the peek popover. The set
  // is empty when no object-targeting is in progress.
  const objectsMapForTargets = gameState?.objects;
  const legalObjectTargetsByController = useMemo(() => {
    const map = new Map<PlayerId, ObjectId[]>();
    if (!objectsMapForTargets) return map;
    for (const tgt of currentLegalTargets) {
      if (!("Object" in tgt)) continue;
      const obj = objectsMapForTargets[tgt.Object];
      if (!obj) continue;
      const list = map.get(obj.controller) ?? [];
      list.push(tgt.Object);
      map.set(obj.controller, list);
    }
    return map;
  }, [currentLegalTargets, objectsMapForTargets]);

  const handlePlayerTarget = useCallback(
    (targetPlayerId: number) => {
      dispatch({ type: "ChooseTarget", data: { target: { Player: targetPlayerId } } });
    },
    [dispatch],
  );
  const handleSelectFocus = useCallback(
    (opId: PlayerId) => {
      if (
        followActiveOpponent
        && activeFollowedOpponent != null
        && opId !== activeFollowedOpponent
      ) {
        setFollowActiveOpponent(false);
      }
      setFocusedOpponent(opId);
    },
    [
      activeFollowedOpponent,
      followActiveOpponent,
      setFocusedOpponent,
      setFollowActiveOpponent,
    ],
  );
  const handleToggleFollowActiveOpponent = useCallback(() => {
    setFollowActiveOpponent(!followActiveOpponent);
  }, [followActiveOpponent, setFollowActiveOpponent]);

  const disconnectedPlayers = useMultiplayerStore((s) => s.disconnectedPlayers);
  const connectionStatus = useMultiplayerStore((s) => s.connectionStatus);
  const isOnline = connectionStatus !== "disconnected";

  const primaryOpponentId = allOpponents[0] ?? (playerId === 0 ? 1 : 0);
  const primaryOpponentAvatarUrl = useMultiplayerStore(
    (s) => s.playerAvatars.get(primaryOpponentId) ?? null,
  );
  // Always-called hook (rules-of-hooks) — used only on the 1v1 branch below.
  const primaryOpponentDesignations = usePlayerDesignations(primaryOpponentId);

  if (!isMultiplayer) {
    // 1v1: single opponent pill (existing design)
    const opponentId = primaryOpponentId;
    const isOpponentTurn = gameState?.active_player === opponentId;
    const isValidTarget = validPlayerTargetIds.includes(opponentId);
    const opponentCompanion = gameState?.players[opponentId]?.companion;
    const opponentSpeed = gameState?.players[opponentId]?.speed ?? 0;
    const opponentPoisonCounters = gameState?.players[opponentId]?.poison_counters ?? 0;
    const opponentRadCounters = gameState?.players[opponentId]?.player_counters?.Rad ?? 0;
    const opponentDesignations = primaryOpponentDesignations;
    const isDisconnected = isOnline && disconnectedPlayers.has(opponentId);
    const isOpponentPhasedOut =
      gameState?.players[opponentId]?.status?.type === "PhasedOut";
    const showMatchScore = gameState?.match_config?.match_type === "Bo3";
    const matchScore = showMatchScore ? gameState?.match_score ?? null : null;
    const label = opponentName ?? getOpponentDisplayName(opponentId);
    const opponentAvatarUrl = primaryOpponentAvatarUrl;

    const hudTone = isValidTarget ? "cyan" : isOpponentTurn ? "rose" : "neutral";
    const opponentSeatColor = getSeatColor(opponentId, gameState?.seat_order);
    const isOpponentUnderAttack = gameState?.combat?.attackers.some(
      (a) => a.attack_target.type === "Player" && a.attack_target.data === opponentId,
    ) ?? false;

    return (
      <div
        data-player-hud={String(opponentId)}
        data-phased-out={isOpponentPhasedOut ? "true" : undefined}
        className={`relative flex items-center gap-1.5 py-1 ${
          isOpponentPhasedOut ? "opacity-40 grayscale" : ""
        }`}
      >
        <HudPlate
          label={label}
          tone={hudTone}
          active={isOpponentTurn}
          seatColor={opponentSeatColor}
          underAttack={isOpponentUnderAttack}
          avatarUrl={opponentAvatarUrl}
          playerId={opponentId}
          onClick={isValidTarget ? () => handlePlayerTarget(opponentId) : undefined}
          trailing={
            <>
              <EnchantmentsBadge playerId={opponentId} />
              {matchScore ? <ScoreBadge score={matchScore} player={1} /> : null}
              {opponentDesignations.isMonarch ? <MonarchBadge /> : null}
              {opponentDesignations.hasInitiative ? <InitiativeBadge /> : null}
              {opponentDesignations.hasCityBlessing ? <CityBlessingBadge /> : null}
              {opponentDesignations.activeDungeon ? (
                <DungeonBadge dungeonName={opponentDesignations.activeDungeon} roomIndex={opponentDesignations.currentRoom} />
              ) : null}
              {isOpponentPhasedOut ? <StatusBadge label={t("player.phasedOut")} tone="neutral" /> : null}
              {opponentDesignations.ringLevel > 0 ? <CounterBadge kind="ring" value={opponentDesignations.ringLevel} /> : null}
              {opponentDesignations.energy > 0 ? <CounterBadge kind="energy" value={opponentDesignations.energy} /> : null}
              {opponentPoisonCounters > 0 ? <CounterBadge kind="poison" value={opponentPoisonCounters} /> : null}
              {opponentRadCounters > 0 ? <CounterBadge kind="rad" value={opponentRadCounters} /> : null}
              {opponentSpeed > 0 ? <CounterBadge kind="speed" value={opponentSpeed} /> : null}
              {opponentCompanion ? <StatusBadge label={t("badges.companion")} /> : null}
              {isOnline ? <ConnectionDotInline disconnected={isDisconnected} /> : null}
            </>
          }
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
    // Single-row opponent rail. Tabs flex to share the available width — they
    // never wrap or scroll off-screen. Each tab is its own query container
    // (`@container`), so its contents progressively disclose: board-composition
    // stats appear only once a tab earns the width, then collapse back to
    // avatar + name + life when the row is squeezed by additional opponents on
    // a narrow (mobile) viewport. `max-w` on each tab caps its size so one or
    // two opponents don't balloon on desktop. KickConfirmDialog is a fixed
    // overlay, so its position in the flow is irrelevant.
    <div className="flex w-full items-center justify-center gap-1.5 px-2 py-1">
      {allOpponents.map((opId) => (
        <OpponentTab
          key={opId}
          playerId={opId}
          isFocused={focusedId === opId}
          isEliminated={eliminated.includes(opId)}
          isTeammate={teamBased && isTeammate(playerId, opId)}
          isValidTarget={validPlayerTargetIds.includes(opId)}
          isTargeting={isTargeting}
          legalObjectTargetIds={legalObjectTargetsByController.get(opId) ?? EMPTY_OBJECT_IDS}
          showMana={focusedId === opId}
          incomingAttackerIds={incomingByOpponent.get(opId) ?? EMPTY_OBJECT_IDS}
          onSelectFocus={() => handleSelectFocus(opId)}
          onTargetPlayer={() => handlePlayerTarget(opId)}
          onKick={
            onKickPlayer && !eliminated.includes(opId)
              ? () => setKickTarget(opId)
              : undefined
          }
        />
      ))}
      <FollowActiveToggle
        enabled={followActiveOpponent}
        onToggle={handleToggleFollowActiveOpponent}
      />
      <KickConfirmDialog
        isOpen={kickTarget !== null}
        playerLabel={targetLabel}
        onConfirm={() => {
          if (kickTarget !== null && onKickPlayer) onKickPlayer(kickTarget);
          setKickTarget(null);
        }}
        onCancel={() => setKickTarget(null)}
      />
    </div>
  );
}

function FollowActiveToggle({
  enabled,
  onToggle,
}: {
  enabled: boolean;
  onToggle: () => void;
}) {
  const { t } = useTranslation("game");
  const tooltipId = useId();
  const tooltip = enabled
    ? t("opponentHud.followingActiveOpponent")
    : t("opponentHud.followActiveOpponent");

  return (
    <button
      type="button"
      aria-label={tooltip}
      aria-describedby={tooltipId}
      aria-pressed={enabled}
      onClick={onToggle}
      className={`group relative flex h-9 w-9 shrink-0 items-center justify-center rounded-full border backdrop-blur-xl transition-all duration-200 ${
        enabled
          ? "border-amber-300/45 bg-amber-500/18 text-amber-100 shadow-[0_0_18px_rgba(245,158,11,0.24)]"
          : "border-white/10 bg-slate-950/62 text-slate-300 hover:border-white/20 hover:text-white"
      }`}
    >
      <span
        aria-hidden
        className={`relative flex h-[18px] w-[18px] items-center justify-center rounded-full border ${
          enabled ? "border-amber-200" : "border-current"
        }`}
      >
        <span className="absolute left-1/2 top-0 h-full w-px -translate-x-1/2 bg-current opacity-75" />
        <span className="absolute left-0 top-1/2 h-px w-full -translate-y-1/2 bg-current opacity-75" />
        <span
          className={`h-1.5 w-1.5 rounded-full ${
            enabled ? "bg-amber-200 shadow-[0_0_8px_rgba(251,191,36,0.85)]" : "bg-current"
          }`}
        />
      </span>
      <span
        id={tooltipId}
        role="tooltip"
        className="pointer-events-none absolute right-0 bottom-full z-50 mb-2 hidden w-64 rounded-md border border-white/10 bg-slate-950/95 px-3 py-2 text-left text-[11px] leading-snug font-medium text-slate-100 shadow-2xl shadow-black/40 backdrop-blur-xl group-hover:block group-focus-visible:block"
      >
        {tooltip}
      </span>
    </button>
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
  /** This opponent (the player) is a legal target right now. Drives the
   *  avatar's pulsing-crosshair target overlay and the tab's informational
   *  cyan accent. Distinct from `isTargeting` — the local player can be in
   *  a target-selection state where no player is legal but some of this
   *  opponent's permanents are. */
  isValidTarget: boolean;
  /** The local player is currently in a target-selection state (either
   *  human, trigger, or copy-retarget). When true, hovering this tab opens
   *  the battlefield peek popover so the targeter can read this opponent's
   *  board without committing to a focus switch. */
  isTargeting: boolean;
  /** Object ids legal to target right now that are controlled by this
   *  opponent. Empty when no object-targeting is active or when none of
   *  this opponent's permanents are legal. */
  legalObjectTargetIds: readonly ObjectId[];
  showMana: boolean;
  /** Attacker object ids this opponent has declared against me / my stuff.
   *  When non-empty, the tab renders a red ⚔×N badge and a hover popover
   *  with mini card images so the defender can assess incoming threats
   *  without first focusing this opponent's board. */
  incomingAttackerIds: readonly ObjectId[];
  /** Tab body click — always navigates focus, never targets. */
  onSelectFocus: () => void;
  /** Avatar click — fires `ChooseTarget` for this opponent. Only invoked
   *  when `isValidTarget`; the avatar is non-interactive otherwise. */
  onTargetPlayer: () => void;
  /** Host-only: when provided, render a small kick affordance on the tab. */
  onKick?: () => void;
}

function OpponentTab({ playerId, isFocused, isEliminated, isTeammate: ally, isValidTarget, isTargeting, legalObjectTargetIds, showMana, incomingAttackerIds, onSelectFocus, onTargetPlayer, onKick }: OpponentTabProps) {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  const isTheirTurn = gameState?.active_player === playerId;
  const seatColor = getSeatColor(playerId, gameState?.seat_order);
  const isUnderAttack = gameState?.combat?.attackers.some(
    (a) => a.attack_target.type === "Player" && a.attack_target.data === playerId,
  ) ?? false;
  const [hoverPopover, setHoverPopover] = useState<"none" | "incoming" | "peek">("none");
  const hasIncoming = incomingAttackerIds.length > 0;
  const battlefieldPeekOnHover = usePreferencesStore((s) => s.battlefieldPeekOnHover);
  // Peek opens for any non-focused opponent on hover — a permanent scout
  // affordance — gated by user preference. Incoming-attackers popover
  // takes precedence during combat (when not in a target-selection state)
  // because the block-planning keyword detail it surfaces is the more
  // tactically relevant view. Incoming runs independently of the peek
  // preference: it's a different kind of affordance (threat surfacing,
  // not exploration) and disabling peek shouldn't hide imminent attacks.
  const peekEligible = !isFocused && battlefieldPeekOnHover;
  const showIncomingOnHover = hasIncoming && !isFocused && !isTargeting;
  const showPeekOnHover = peekEligible && !showIncomingOnHover;
  const hoverEnabled = showPeekOnHover || showIncomingOnHover;
  const tabRef = useRef<HTMLButtonElement>(null);
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
    // Peek wins over incoming when both apply — peek is the more relevant
    // affordance while the local player is actively choosing targets.
    setHoverPopover(showPeekOnHover ? "peek" : showIncomingOnHover ? "incoming" : "none");
  }, [showPeekOnHover, showIncomingOnHover]);
  const scheduleClosePopover = useCallback(() => {
    if (closeTimerRef.current != null) window.clearTimeout(closeTimerRef.current);
    closeTimerRef.current = window.setTimeout(() => {
      setHoverPopover("none");
      closeTimerRef.current = null;
    }, 180);
  }, []);
  useEffect(() => () => {
    if (closeTimerRef.current != null) window.clearTimeout(closeTimerRef.current);
  }, []);
  // When targeting state changes mid-hover, the open popover may no longer
  // apply (e.g., player committed a target while hovering). Close so the
  // next mouseenter recomputes which popover should open.
  useEffect(() => {
    if (!hoverEnabled && hoverPopover !== "none") setHoverPopover("none");
  }, [hoverEnabled, hoverPopover]);
  const player = gameState?.players[playerId];
  const isDisconnected = useMultiplayerStore((s) => s.disconnectedPlayers.has(playerId));
  const isOnline = useMultiplayerStore((s) => s.connectionStatus) !== "disconnected";
  const avatarUrl = useMultiplayerStore((s) => s.playerAvatars.get(playerId) ?? null);
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

  // Hoisted above the early return (rules-of-hooks).
  const designations = usePlayerDesignations(playerId);

  if (!player) return null;

  const handCount = player.hand.length;
  const speed = player.speed ?? 0;
  const poisonCounters = player.poison_counters;
  const radCounters = player.player_counters?.Rad ?? 0;
  const isPhasedOut = player.status?.type === "PhasedOut";

  const label = ally ? t("opponentHud.ally") : getOpponentDisplayName(playerId);

  // Two-step click for player-targeting (Option B at the tab level):
  //   - Unfocused tab click → focus this opponent (navigate).
  //   - Focused + targetable click → commit target on the player (commit).
  //   - Focused + not targetable click → no-op (already viewing).
  // The visual affordance reflects which step the next click will perform:
  // cyan accent on any targetable opponent, but the prominent commit-ready
  // treatment (crosshair cursor, pulsing glow, bright ring) appears only
  // when also focused — so the user gets clear "the next click commits"
  // feedback before they pull the trigger.
  const commitReady = isValidTarget && isFocused;
  const borderClass = commitReady
    ? "border-cyan-300/70 bg-cyan-950/40 ring-2 ring-cyan-300/70 shadow-[0_0_22px_rgba(34,211,238,0.55)] cursor-crosshair"
    : isValidTarget
      ? "border-cyan-400/45 bg-cyan-950/30 ring-1 ring-cyan-300/35"
      : isTheirTurn
        ? "border-rose-400/45 bg-rose-950/40 ring-2 ring-rose-300/70 ring-offset-2 ring-offset-black/40 shadow-[0_14px_28px_rgba(244,63,94,0.22)]"
        : ally
          ? isFocused
            ? "border-emerald-400/40 bg-emerald-950/40 ring-1 ring-emerald-300/30"
            : "border-emerald-700/40 bg-slate-950/70 hover:border-emerald-400/40 hover:bg-slate-900/72"
          : isFocused
            ? "border-amber-400/40 bg-amber-950/38 ring-1 ring-amber-300/30"
            : "border-white/10 bg-slate-950/70 hover:border-white/20 hover:bg-slate-900/72";

  const ariaLabel = commitReady
    ? t("opponentHud.targetPlayer", { name: label })
    : isValidTarget
      ? t("opponentHud.viewBoardThenTarget", { name: label })
      : t("opponentHud.viewBoard", { name: label });
  const titleTooltip = commitReady
    ? t("opponentHud.clickToTarget", { name: label })
    : isValidTarget
      ? t("opponentHud.clickToViewThenTarget", { name: label })
      : t("opponentHud.clickToViewBoard", { name: label });
  const onTabClick = commitReady ? onTargetPlayer : onSelectFocus;

  return (
    <button
      ref={tabRef}
      type="button"
      onClick={onTabClick}
      disabled={isEliminated}
      aria-label={ariaLabel}
      title={titleTooltip}
      data-player-hud={String(playerId)}
      data-phased-out={isPhasedOut ? "true" : undefined}
      onMouseEnter={hoverEnabled ? openPopover : undefined}
      onMouseLeave={hoverEnabled ? scheduleClosePopover : undefined}
      onFocus={hoverEnabled ? openPopover : undefined}
      onBlur={hoverEnabled ? scheduleClosePopover : undefined}
      // `@container`: each tab is its own query context so its descendants
      // (avatar, name, stats) can size/reveal based on this tab's width — which
      // is the row width divided by the opponent count. `flex-1 min-w-0` lets
      // tabs shrink to share one row; `max-w` caps a tab so 1-2 opponents don't
      // balloon. Chrome (padding/gap) is fixed because container queries can't
      // target the container element itself, only its descendants.
      //
      // The cap MUST stay >= the width the full breakdown reveal needs (the
      // `@min-[19rem]` gate below), otherwise a capped tab reveals stats it has
      // no room for and the life total spills over the HAND column. The stat
      // labels are px-sized (don't scale with rem), so the full breakdown
      // (avatar + name/life + HAND + creatures/lands/other) measures ~18rem
      // (~290px at the default 16px root, verified in-browser). Cap at 20rem
      // gives headroom for a 2-digit life and a designation badge; the reveal is
      // gated at 19rem so a tab too narrow to fit the breakdown collapses to the
      // HAND-only tier (tap to focus) instead of overlapping.
      className={`@container relative flex min-w-0 max-w-[20rem] flex-1 items-center gap-1.5 rounded-lg border px-1.5 py-1 backdrop-blur-xl transition-all duration-200 ${borderClass} ${isEliminated || isPhasedOut ? "opacity-40 grayscale" : ""}`}
    >
      {isTheirTurn && !shouldReduceMotion && !commitReady && (
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
      {commitReady && !shouldReduceMotion && (
        <motion.div
          aria-hidden
          className="pointer-events-none absolute -inset-0.5 rounded-[20px]"
          animate={{
            boxShadow: [
              "0 0 0 0 rgba(34, 211, 238, 0.45), 0 0 14px 2px rgba(34, 211, 238, 0.45)",
              "0 0 0 2px rgba(34, 211, 238, 0.8), 0 0 28px 8px rgba(34, 211, 238, 0.7)",
            ],
          }}
          transition={{
            duration: 1.1,
            repeat: Infinity,
            repeatType: "reverse",
            ease: "easeInOut",
          }}
        />
      )}
      {isUnderAttack && (
        <>
          <UnderAttackOverlay />
          <span className="sr-only">{t("opponentHud.underAttack", { name: label })}</span>
        </>
      )}
      <OpponentAvatar
        label={label}
        avatarUrl={avatarUrl}
        seatColor={seatColor}
      />
      {/* `overflow-hidden`: structural guard so the life total / designation
          badges can never spill out of this flex-1 column onto the HAND stat
          (the overlap bug). With the breakdown gated to only reveal when it
          fits, this stays a safety net rather than clipping in normal play. */}
      <div className="flex min-w-0 flex-1 flex-col items-start overflow-hidden leading-none">
        <span
          className="relative mb-0.5 flex w-full min-w-0 items-center gap-1 text-[9px] font-semibold uppercase tracking-[0.1em] @min-[11rem]:mb-1 @min-[11rem]:text-[10px] @min-[11rem]:tracking-[0.18em]"
          style={{ color: seatColor }}
        >
          <span className="truncate">{label}</span>
        </span>
        <div className="flex items-center gap-1">
          {isTheirTurn && <span className="h-1.5 w-1.5 rounded-full bg-rose-400 animate-pulse" />}
          <span className={`text-xs font-semibold @min-[10rem]:text-sm ${isTheirTurn ? "text-rose-200" : ally ? "text-emerald-200" : isFocused ? "text-amber-100" : "text-slate-100"}`}>
            {player.life}
          </span>
          {designations.isMonarch ? <MonarchBadge /> : null}
          {designations.hasInitiative ? <InitiativeBadge /> : null}
          {designations.hasCityBlessing ? <CityBlessingBadge /> : null}
          {designations.activeDungeon ? (
            <DungeonBadge dungeonName={designations.activeDungeon} roomIndex={designations.currentRoom} />
          ) : null}
          {designations.ringLevel > 0 ? <CounterBadge kind="ring" value={designations.ringLevel} /> : null}
          {designations.energy > 0 ? <CounterBadge kind="energy" value={designations.energy} /> : null}
          {poisonCounters > 0 ? <CounterBadge kind="poison" value={poisonCounters} /> : null}
          {radCounters > 0 ? <CounterBadge kind="rad" value={radCounters} /> : null}
          {speed > 0 ? <CounterBadge kind="speed" value={speed} /> : null}
          {isOnline && <ConnectionDotInline disconnected={isDisconnected} />}
        </div>
      </div>

      {/* Progressive disclosure (container-query driven, keyed off this tab's
          width): hand count reveals once the tab has a little room, the full
          board-composition breakdown only when it's comfortably wide. When
          squeezed (more opponents / narrow viewport) these collapse and the
          player taps the tab to focus that opponent's full board. */}
      <div className="hidden shrink-0 @min-[7rem]:flex">
        <Stat label={t("opponentHud.statHand")} value={handCount} color="text-slate-200" />
      </div>
      <div className="hidden shrink-0 items-center gap-1.5 @min-[19rem]:flex">
        {counts.creatures > 0 && <Stat label={t("opponentHud.statCreatures")} value={counts.creatures} color="text-rose-200" />}
        {counts.lands > 0 && <Stat label={t("opponentHud.statLands")} value={counts.lands} color="text-emerald-200" />}
        {counts.other > 0 && <Stat label={t("opponentHud.statOther")} value={counts.other} color="text-cyan-200" />}
      </div>

      {player.companion != null && (
        <StatusBadge label={t("badges.companion")} tone={player.companion.used ? "neutral" : "amber"} />
      )}

      {showMana && <ManaPoolSummary playerId={playerId} />}

      {isEliminated && (
        <span className="rounded-full bg-red-900/60 px-2 py-1 text-[10px] font-bold uppercase tracking-[0.16em] text-red-300">{t("opponentHud.out")}</span>
      )}

      {isPhasedOut && !isEliminated && (
        <span className="rounded-full bg-indigo-900/60 px-2 py-1 text-[10px] font-bold uppercase tracking-[0.16em] text-indigo-200">{t("opponentHud.phased")}</span>
      )}

      {onKick && !isEliminated && (
        // Stop propagation so clicking the kick affordance doesn't also fire
        // the parent button's `onClick` (which sets focused opponent or
        // selects a target).
        <span
          role="button"
          tabIndex={0}
          aria-label={t("opponentHud.kickPlayer", { seat: playerId + 1 })}
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
          title={t("opponentHud.kickPlayerTooltip")}
        >
          ×
        </span>
      )}
      {/* Cross-board attacker badge — left-positioned to avoid colliding
          with the right-edge kick `×` affordance rendered above. The badge
          stays even while the peek popover is showing instead, so the
          defender doesn't lose track of incoming threats during targeting. */}
      {hasIncoming && (
        <span
          aria-label={t("opponentHud.incomingAttackers", { count: incomingAttackerIds.length })}
          className={`absolute -left-1.5 -top-1.5 z-10 flex h-5 min-w-5 items-center justify-center rounded-full bg-red-600 px-1 text-[10px] font-bold text-white shadow ring-2 ring-red-300 ${shouldReduceMotion ? "" : "animate-pulse"}`}
        >
          ⚔×{incomingAttackerIds.length}
        </span>
      )}
      {hoverPopover === "incoming" && tabRef.current && (
        <PortaledPopover anchorEl={tabRef.current}>
          <IncomingAttackersPopover
            attackerIds={incomingAttackerIds}
            opponentName={label}
          />
        </PortaledPopover>
      )}
      {hoverPopover === "peek" && tabRef.current && (
        <PortaledPopover anchorEl={tabRef.current}>
          <BattlefieldPeekPopover
            playerId={playerId}
            opponentName={label}
            seatColor={seatColor}
            isTargeting={isTargeting}
            legalTargetIds={legalObjectTargetIds}
          />
        </PortaledPopover>
      )}
    </button>
  );
}

function OpponentAvatar({
  label,
  avatarUrl,
  seatColor,
}: {
  label: string;
  avatarUrl: string | null;
  seatColor: string;
}) {
  // Inner avatar visuals: real portrait when known, synthesized
  // seat-color tile with the player's initial otherwise.
  const inner = avatarUrl ? (
    <>
      <img src={avatarUrl} alt={label} className="h-full w-full object-cover" />
      <div className="absolute inset-0 bg-gradient-to-b from-white/12 via-transparent to-black/35" />
    </>
  ) : (
    <>
      <div
        className="flex h-full w-full items-center justify-center text-[11px] font-bold text-white/90 @min-[11rem]:text-sm"
        style={{ backgroundColor: `${seatColor}55` }}
      >
        {label.charAt(0).toUpperCase()}
      </div>
      <div className="absolute inset-0 bg-gradient-to-b from-white/10 via-transparent to-black/40" />
    </>
  );

  // Avatar scales with the tab's width (container query): compact on a squeezed
  // mobile tab, full-size once the tab is comfortably wide. Smaller height here
  // is what keeps the rail short enough to clear the cards above it on mobile.
  const tileClassName = "relative h-8 w-7 shrink-0 overflow-hidden rounded-md border border-white/15 bg-slate-950 shadow-[0_8px_18px_rgba(0,0,0,0.32)] @min-[11rem]:h-10 @min-[11rem]:w-9 @min-[11rem]:rounded-lg";
  const tileStyle: CSSProperties = {
    borderColor: `${seatColor}cc`,
    boxShadow: `0 0 0 1px ${seatColor}55, 0 8px 18px rgba(0,0,0,0.32), 0 0 14px ${seatColor}2e`,
  };

  if (!avatarUrl) {
    return <div className={tileClassName} style={tileStyle}>{inner}</div>;
  }

  return (
    <AvatarHoverPreview
      avatarUrl={avatarUrl}
      label={label}
      seatColor={seatColor}
      className={tileClassName}
      style={tileStyle}
    >
      {inner}
    </AvatarHoverPreview>
  );
}

function ConnectionDotInline({ disconnected }: { disconnected: boolean }) {
  const { t } = useTranslation("game");
  return (
    <span
      className={`inline-block h-2 w-2 rounded-full ring-1 ring-white/20 ${disconnected ? "bg-red-500 animate-pulse" : "bg-emerald-400"}`}
      title={disconnected ? t("opponentHud.disconnected") : t("opponentHud.connected")}
    />
  );
}

function PortaledPopover({ anchorEl, children }: { anchorEl: HTMLElement; children: React.ReactNode }) {
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const stableCountRef = useRef(0);

  useEffect(() => {
    stableCountRef.current = 0;
    let prevLeft = 0;
    let prevTop = 0;
    let rafId: number;

    function poll() {
      const rect = anchorEl.getBoundingClientRect();
      const left = rect.left + rect.width / 2;
      const top = rect.bottom + 8;
      const changed = Math.abs(left - prevLeft) > 0.5 || Math.abs(top - prevTop) > 0.5;
      prevLeft = left;
      prevTop = top;
      stableCountRef.current = changed ? 0 : stableCountRef.current + 1;
      setPos({ left, top });

      if (stableCountRef.current < 10) {
        rafId = requestAnimationFrame(poll);
      }
    }

    rafId = requestAnimationFrame(poll);
    return () => cancelAnimationFrame(rafId);
  }, [anchorEl]);

  if (!pos) return null;

  return createPortal(
    <div
      className="pointer-events-none fixed z-40"
      style={{ left: pos.left, top: pos.top, transform: "translateX(-50%)" }}
    >
      {children}
    </div>,
    document.body,
  );
}

// Labels are a single compact px size on purpose: a wider-label tier would
// inflate the full breakdown past the tab cap (px labels don't scale with the
// rem cap), re-triggering the life-over-HAND overlap. Width budget for the
// reveal gate / cap above is sized against this size.
function Stat({ label, value, color }: { label: string; value: number; color: string }) {
  return (
    <div className="flex flex-col items-start leading-none">
      <span className="mb-0.5 text-[8px] font-medium uppercase tracking-[0.12em] text-white/40">{label}</span>
      <span className={`text-xs font-semibold tabular-nums ${color}`}>{value}</span>
    </div>
  );
}
