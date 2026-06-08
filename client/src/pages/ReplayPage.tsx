import { useCallback, useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router";

import type { GameState } from "../adapter/types.ts";
import { ReplayBattlefield } from "../components/replay/ReplayBattlefield.tsx";
import { ReplayControls } from "../components/replay/ReplayControls.tsx";
import { ReplayPlayerPanel } from "../components/replay/ReplayPlayerPanel.tsx";
import { loadCheckpoints } from "../stores/gameStore.ts";

const PHASE_LABEL: Record<string, string> = {
  Untap: "Untap",
  Upkeep: "Upkeep",
  Draw: "Draw",
  PreCombatMain: "Main Phase 1",
  BeginCombat: "Begin Combat",
  DeclareAttackers: "Declare Attackers",
  DeclareBlockers: "Declare Blockers",
  CombatDamage: "Combat Damage",
  EndCombat: "End Combat",
  PostCombatMain: "Main Phase 2",
  End: "End Step",
  Cleanup: "Cleanup",
};

function phaseLabel(phase: string): string {
  return PHASE_LABEL[phase] ?? phase;
}

interface ReplayBoardProps {
  state: GameState;
  viewerPlayerId: number;
}

function ReplayBoard({ state, viewerPlayerId }: ReplayBoardProps) {
  const players = state.players;
  // viewer at bottom, opponent(s) at top
  const viewer = players.find((p) => p.id === viewerPlayerId) ?? players[0];
  const opponents = players.filter((p) => p.id !== viewer.id);

  return (
    <div className="flex flex-col gap-3 flex-1">
      {/* Opponents */}
      {opponents.map((opp) => (
        <div key={opp.id} className="flex flex-col gap-1">
          <ReplayPlayerPanel
            player={opp}
            activePlayerId={viewerPlayerId}
            state={state}
            side="top"
          />
          <ReplayBattlefield
            state={state}
            playerId={opp.id}
            label={`P${opp.id + 1} battlefield`}
          />
        </div>
      ))}

      {/* Phase indicator */}
      <div className="flex items-center justify-center gap-3 py-2 border-y border-[#333] text-sm">
        <span className="text-[#888]">Turn {state.turn_number}</span>
        <span className="text-[#555]">·</span>
        <span className="text-[#aaa]">
          Active: <span className="text-white">P{state.active_player + 1}</span>
        </span>
        <span className="text-[#555]">·</span>
        <span className="text-[#8b5cf6]">{phaseLabel(state.phase)}</span>
      </div>

      {/* Viewer */}
      <div className="flex flex-col gap-1">
        <ReplayBattlefield
          state={state}
          playerId={viewer.id}
          label="Your battlefield"
        />
        <ReplayPlayerPanel
          player={viewer}
          activePlayerId={viewerPlayerId}
          state={state}
          side="bottom"
        />
      </div>
    </div>
  );
}

export function ReplayPage() {
  const { gameId } = useParams<{ gameId: string }>();
  const navigate = useNavigate();

  const [checkpoints, setCheckpoints] = useState<GameState[]>([]);
  const [currentIndex, setCurrentIndex] = useState(0);
  const [isPlaying, setIsPlaying] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!gameId) {
      setError("No game ID specified.");
      setLoading(false);
      return;
    }
    loadCheckpoints(gameId)
      .then((cps) => {
        if (cps.length === 0) {
          setError("No replay data found for this game.");
        } else {
          setCheckpoints(cps);
          setCurrentIndex(0);
        }
      })
      .catch(() => setError("Failed to load replay data."))
      .finally(() => setLoading(false));
  }, [gameId]);

  const handlePrev = useCallback(() => {
    setCurrentIndex((i) => Math.max(0, i - 1));
    setIsPlaying(false);
  }, []);

  const handleNext = useCallback(() => {
    setCurrentIndex((i) => {
      const next = i + 1;
      if (next >= checkpoints.length) {
        setIsPlaying(false);
        return i;
      }
      return next;
    });
  }, [checkpoints.length]);

  const handleSeek = useCallback((index: number) => {
    setCurrentIndex(index);
    setIsPlaying(false);
  }, []);

  const handlePlayPause = useCallback(() => {
    if (currentIndex >= checkpoints.length - 1) {
      // Restart from beginning
      setCurrentIndex(0);
      setIsPlaying(true);
    } else {
      setIsPlaying((p) => !p);
    }
  }, [currentIndex, checkpoints.length]);

  const current = checkpoints[currentIndex];

  return (
    <div className="min-h-screen bg-[#111] text-white flex flex-col">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-[#333] bg-[#161616]">
        <button
          onClick={() => navigate(-1)}
          className="text-sm text-[#888] hover:text-white transition-colors"
        >
          ← Back
        </button>
        <h1 className="text-base font-semibold text-[#ddd]">
          Game Replay{gameId ? ` · ${gameId.slice(0, 8)}…` : ""}
        </h1>
        <div className="w-16" />
      </div>

      <div className="flex-1 flex flex-col gap-4 p-4 max-w-3xl mx-auto w-full">
        {loading && (
          <div className="flex-1 flex items-center justify-center text-[#888]">
            Loading replay…
          </div>
        )}

        {error && !loading && (
          <div className="flex-1 flex flex-col items-center justify-center gap-4">
            <p className="text-red-400">{error}</p>
            <button
              onClick={() => navigate("/")}
              className="px-4 py-2 bg-[#2a2a2a] rounded text-sm hover:bg-[#3a3a3a]"
            >
              Go Home
            </button>
          </div>
        )}

        {!loading && !error && current && (
          <>
            <ReplayControls
              currentIndex={currentIndex}
              total={checkpoints.length}
              isPlaying={isPlaying}
              onPrev={handlePrev}
              onNext={handleNext}
              onSeek={handleSeek}
              onPlayPause={handlePlayPause}
            />
            <ReplayBoard
              state={current}
              viewerPlayerId={current.players[0]?.id ?? 0}
            />
          </>
        )}
      </div>
    </div>
  );
}
