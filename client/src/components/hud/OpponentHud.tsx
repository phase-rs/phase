import { useMemo } from "react";

import type { PlayerId } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
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
  const gameState = useGameStore((s) => s.gameState);

  const allOpponents = useMemo(() => {
    if (!gameState) return [];
    const seatOrder = gameState.seat_order ?? gameState.players.map((p) => p.id);
    return seatOrder.filter((id) => id !== playerId);
  }, [gameState, playerId]);

  const eliminated = gameState?.eliminated_players ?? [];
  const liveOpponents = allOpponents.filter((id) => !eliminated.includes(id));
  const isMultiplayer = allOpponents.length > 1;

  if (!isMultiplayer) {
    // 1v1: single opponent pill (existing design)
    const opponentId = allOpponents[0] ?? (playerId === 0 ? 1 : 0);
    const isOpponentTurn = gameState?.active_player === opponentId;
    const label = opponentName ?? `Opp ${opponentId + 1}`;

    return (
      <div data-player-hud={String(opponentId)} className="flex items-center justify-center py-1">
        <div className={`flex items-center gap-2 rounded-full px-3 py-1 transition-all duration-300 ${isOpponentTurn ? "bg-black/50 ring-[3px] ring-red-400 shadow-[0_0_20px_rgba(248,113,113,0.5),0_0_6px_rgba(248,113,113,0.4)]" : "bg-black/50"}`}>
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
          showMana={focusedId === opId}
          onClick={() => setFocusedOpponent(opId)}
        />
      ))}
    </div>
  );
}

interface OpponentTabProps {
  playerId: PlayerId;
  isFocused: boolean;
  isEliminated: boolean;
  showMana: boolean;
  onClick: () => void;
}

function OpponentTab({ playerId, isFocused, isEliminated, showMana, onClick }: OpponentTabProps) {
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
      other: partition.other.length,
    };
  }, [gameState, playerId]);

  if (!player) return null;

  const handCount = player.hand.length;

  const borderClass = isTheirTurn
    ? "border-red-400 bg-black/60 ring-1 ring-red-400/40 shadow-[0_0_10px_rgba(248,113,113,0.3)]"
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
        <span className={`text-xs font-medium ${isTheirTurn ? "text-red-300" : isFocused ? "text-amber-300" : "text-gray-400"}`}>
          Opp {playerId + 1}
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
