import { useCallback, useEffect, useMemo } from "react";

import type { PlayerId } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { partitionByType } from "../../viewmodel/battlefieldProps.ts";
import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";

interface OpponentHudProps {
  opponentName?: string | null;
}

export function OpponentHud({ opponentName }: OpponentHudProps) {
  const playerId = usePlayerId();
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

  if (!isMultiplayer) {
    // 1v1: single opponent pill (existing design)
    const opponentId = allOpponents[0] ?? (playerId === 0 ? 1 : 0);
    const isOpponentTurn = gameState?.active_player === opponentId;
    const isValidTarget = validPlayerTargetIds.includes(opponentId);
    const label = opponentName ?? `Opp ${opponentId + 1}`;

    const pillClass = isValidTarget
      ? "bg-black/50 ring-[3px] ring-cyan-400 shadow-[0_0_20px_rgba(34,211,238,0.6),0_0_8px_rgba(34,211,238,0.4)] cursor-pointer"
      : isOpponentTurn
        ? "bg-black/50 ring-[3px] ring-red-400 shadow-[0_0_20px_rgba(248,113,113,0.5),0_0_6px_rgba(248,113,113,0.4)]"
        : "bg-black/50";

    return (
      <div data-player-hud={String(opponentId)} className="flex items-center justify-center py-1">
        <div
          onClick={isValidTarget ? () => handlePlayerTarget(opponentId) : undefined}
          className={`flex items-center gap-2 rounded-full px-3 py-1 transition-all duration-300 ${pillClass}`}
        >
          <span className="text-xs font-medium text-gray-400">{label}</span>
          <LifeTotal playerId={opponentId} size="lg" hideLabel />
          <ManaPoolSummary playerId={opponentId} />
        </div>
      </div>
    );
  }

  // Multiplayer: tabbed opponent selector
  const focusedId = focusedOpponent ?? liveOpponents[0];

  return (
    <div className="flex items-center justify-center gap-1.5 px-2 py-1">
      {allOpponents.map((opId) => (
        <OpponentTab
          key={opId}
          playerId={opId}
          isFocused={focusedId === opId}
          isEliminated={eliminated.includes(opId)}
          isTeammate={teamBased && isTeammate(playerId, opId)}
          isValidTarget={validPlayerTargetIds.includes(opId)}
          showMana={focusedId === opId}
          onClick={() => validPlayerTargetIds.includes(opId) ? handlePlayerTarget(opId) : setFocusedOpponent(opId)}
        />
      ))}
      <button
        type="button"
        aria-pressed={followActiveOpponent}
        onClick={() => setFollowActiveOpponent(!followActiveOpponent)}
        className={`rounded-full border px-2.5 py-1 text-[11px] font-medium transition-colors ${
          followActiveOpponent
            ? "border-amber-400 bg-amber-400/15 text-amber-200"
            : "border-gray-600 bg-gray-900/80 text-gray-400 hover:border-gray-400 hover:text-gray-200"
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
  onClick: () => void;
}

function OpponentTab({ playerId, isFocused, isEliminated, isTeammate: ally, isValidTarget, showMana, onClick }: OpponentTabProps) {
  const gameState = useGameStore((s) => s.gameState);
  const isTheirTurn = gameState?.active_player === playerId;
  const player = gameState?.players[playerId];

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

  const label = ally ? "Ally" : `Opp ${playerId + 1}`;

  const borderClass = isValidTarget
    ? "border-cyan-400 bg-black/60 ring-2 ring-cyan-400/60 shadow-[0_0_16px_rgba(34,211,238,0.5)] cursor-pointer"
    : isTheirTurn
      ? "border-red-400 bg-black/60 ring-1 ring-red-400/40 shadow-[0_0_10px_rgba(248,113,113,0.3)]"
      : ally
        ? isFocused
          ? "border-emerald-400 bg-gray-800/90 ring-1 ring-emerald-400/30"
          : "border-emerald-600 bg-gray-900/80 hover:border-emerald-400 hover:bg-gray-800/80"
        : isFocused
          ? "border-amber-400 bg-gray-800/90 ring-1 ring-amber-400/30"
          : "border-gray-600 bg-gray-900/80 hover:border-gray-400 hover:bg-gray-800/80";

  return (
    <button
      type="button"
      onClick={onClick}
      disabled={isEliminated}
      className={`flex items-center gap-2.5 rounded-lg border-2 px-2.5 py-1 transition-all duration-300 ${borderClass} ${isEliminated ? "opacity-40 grayscale" : ""}`}
    >
      {/* Name + turn indicator */}
      <div className="flex items-center gap-1">
        {isTheirTurn && <span className="h-1.5 w-1.5 rounded-full bg-red-400 animate-pulse" />}
        <span className={`text-xs font-medium ${isTheirTurn ? "text-red-300" : ally ? "text-emerald-300" : isFocused ? "text-amber-300" : "text-gray-400"}`}>
          {label}
        </span>
      </div>

      {/* Life */}
      <LifeTotal playerId={playerId} size="default" hideLabel />

      {/* Hand count */}
      <Stat label="Hnd" value={handCount} color="text-gray-300" />

      {/* Permanent counts */}
      {counts.creatures > 0 && <Stat label="Crt" value={counts.creatures} color="text-red-400" />}
      {counts.lands > 0 && <Stat label="Lnd" value={counts.lands} color="text-green-400" />}
      {counts.other > 0 && <Stat label="Oth" value={counts.other} color="text-blue-400" />}

      {/* Mana pool — focused tab only */}
      {showMana && <ManaPoolSummary playerId={playerId} />}

      {/* Eliminated badge */}
      {isEliminated && (
        <span className="rounded bg-red-900/60 px-1.5 py-0.5 text-[10px] font-bold text-red-300">OUT</span>
      )}
    </button>
  );
}

function Stat({ label, value, color }: { label: string; value: number; color: string }) {
  return (
    <div className="flex flex-col items-center leading-none">
      <span className="text-[9px] text-gray-500">{label}</span>
      <span className={`text-sm font-medium tabular-nums ${color}`}>{value}</span>
    </div>
  );
}
