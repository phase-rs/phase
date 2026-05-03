/**
 * Draft Pod Page — P2P multiplayer draft flow.
 *
 * Progressive flow:
 * 1. Setup: host creates or guest joins a pod
 * 2. Lobby: 8-seat grid with bot-fill controls (DraftPodLobby)
 * 3. Drafting: pack display + pool panel (reuses Quick Draft components)
 * 4. Deckbuilding: LimitedDeckBuilder (reuses Quick Draft component)
 */

import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { DraftPodLobby } from "../components/draft/DraftPodLobby";
import { PackDisplay } from "../components/draft/PackDisplay";
import { PoolPanel } from "../components/draft/PoolPanel";
import { DraftProgress } from "../components/draft/DraftProgress";
import { LimitedDeckBuilder } from "../components/draft/LimitedDeckBuilder";
import { SetSelector } from "../components/draft/SetSelector";
import { menuButtonClass } from "../components/menu/buttonStyles";
import {
  useMultiplayerDraftStore,
  type MultiplayerDraftPhase,
} from "../stores/multiplayerDraftStore";
import { useDraftPodStore } from "../stores/draftPodStore";

// ── Setup Mode ────────────────────────────────────────────────────────

type SetupMode = "choose" | "host" | "join";

function PodSetup() {
  const [mode, setMode] = useState<SetupMode>("choose");

  const config = useDraftPodStore((s) => s.config);
  const setConfig = useDraftPodStore((s) => s.setConfig);
  const hostDisplayName = useDraftPodStore((s) => s.hostDisplayName);
  const setHostDisplayName = useDraftPodStore((s) => s.setHostDisplayName);
  const guestDisplayName = useDraftPodStore((s) => s.guestDisplayName);
  const setGuestDisplayName = useDraftPodStore((s) => s.setGuestDisplayName);
  const joinCode = useDraftPodStore((s) => s.joinCode);
  const setJoinCode = useDraftPodStore((s) => s.setJoinCode);
  const createPod = useDraftPodStore((s) => s.createPod);
  const joinPod = useDraftPodStore((s) => s.joinPod);
  const configError = useDraftPodStore((s) => s.configError);
  const loadingPool = useDraftPodStore((s) => s.loadingPool);

  if (mode === "choose") {
    return (
      <div className="mx-auto flex w-full max-w-lg flex-col gap-6">
        <h1 className="text-3xl font-bold text-white">Pod Draft</h1>
        <p className="text-sm text-white/50">
          Host or join a multiplayer draft pod with up to 8 players.
        </p>
        <div className="flex gap-4">
          <button
            onClick={() => setMode("host")}
            className={menuButtonClass({ tone: "emerald", size: "lg" })}
          >
            Host a Pod
          </button>
          <button
            onClick={() => setMode("join")}
            className={menuButtonClass({ tone: "blue", size: "lg" })}
          >
            Join a Pod
          </button>
        </div>
      </div>
    );
  }

  if (mode === "host") {
    return (
      <div className="mx-auto flex w-full max-w-4xl flex-col gap-6">
        <div className="flex items-center gap-4">
          <button
            onClick={() => setMode("choose")}
            className="text-sm text-white/50 hover:text-white/80"
          >
            &larr; Back
          </button>
          <h1 className="text-3xl font-bold text-white">Host a Pod</h1>
        </div>

        {/* Display name */}
        <div className="flex flex-col gap-1">
          <label className="text-sm font-medium text-white/60">
            Display Name
          </label>
          <input
            type="text"
            value={hostDisplayName}
            onChange={(e) => setHostDisplayName(e.target.value)}
            placeholder="Enter your name..."
            className="rounded-lg border border-white/10 bg-black/30 px-4 py-2 text-white placeholder-white/30 outline-none focus:border-emerald-400/40"
          />
        </div>

        {/* Draft type */}
        <div className="flex gap-4">
          <label className="flex items-center gap-2 text-sm text-white/70">
            <input
              type="radio"
              name="draftKind"
              checked={config.kind === "Premier"}
              onChange={() => setConfig({ kind: "Premier" })}
              className="accent-emerald-400"
            />
            Premier (ranked)
          </label>
          <label className="flex items-center gap-2 text-sm text-white/70">
            <input
              type="radio"
              name="draftKind"
              checked={config.kind === "Traditional"}
              onChange={() => setConfig({ kind: "Traditional" })}
              className="accent-emerald-400"
            />
            Traditional (best-of-3)
          </label>
        </div>

        {/* Pod size */}
        <div className="flex flex-col gap-1">
          <label className="text-sm font-medium text-white/60">Pod Size</label>
          <select
            value={config.podSize}
            onChange={(e) => setConfig({ podSize: Number(e.target.value) })}
            className="w-32 rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-white outline-none focus:border-emerald-400/40"
          >
            {[4, 6, 8].map((n) => (
              <option key={n} value={n}>
                {n} players
              </option>
            ))}
          </select>
        </div>

        {/* Set selector — reuse the Quick Draft component */}
        <SetSelector
          onStartDraft={(setCode) => {
            setConfig({ setCode });
            void createPod();
          }}
        />

        {/* Error */}
        {configError && (
          <div className="rounded-lg border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-300">
            {configError}
          </div>
        )}

        {/* Loading */}
        {loadingPool && (
          <div className="text-sm text-white/50">Loading set pool data...</div>
        )}
      </div>
    );
  }

  // mode === "join"
  return (
    <div className="mx-auto flex w-full max-w-lg flex-col gap-6">
      <div className="flex items-center gap-4">
        <button
          onClick={() => setMode("choose")}
          className="text-sm text-white/50 hover:text-white/80"
        >
          &larr; Back
        </button>
        <h1 className="text-3xl font-bold text-white">Join a Pod</h1>
      </div>

      {/* Display name */}
      <div className="flex flex-col gap-1">
        <label className="text-sm font-medium text-white/60">
          Display Name
        </label>
        <input
          type="text"
          value={guestDisplayName}
          onChange={(e) => setGuestDisplayName(e.target.value)}
          placeholder="Enter your name..."
          className="rounded-lg border border-white/10 bg-black/30 px-4 py-2 text-white placeholder-white/30 outline-none focus:border-emerald-400/40"
        />
      </div>

      {/* Room code */}
      <div className="flex flex-col gap-1">
        <label className="text-sm font-medium text-white/60">Room Code</label>
        <input
          type="text"
          value={joinCode}
          onChange={(e) => setJoinCode(e.target.value.toUpperCase())}
          placeholder="Enter room code..."
          className="rounded-lg border border-white/10 bg-black/30 px-4 py-2 font-mono text-lg tracking-wider text-white placeholder-white/30 outline-none focus:border-blue-400/40"
        />
      </div>

      {/* Error */}
      {configError && (
        <div className="rounded-lg border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-300">
          {configError}
        </div>
      )}

      <button
        onClick={() => void joinPod()}
        disabled={!joinCode.trim() || !guestDisplayName.trim()}
        className={menuButtonClass({
          tone: "blue",
          size: "md",
          disabled: !joinCode.trim() || !guestDisplayName.trim(),
        })}
      >
        Join Pod
      </button>
    </div>
  );
}

// ── Phase-based Content ───────────────────────────────────────────────

function phaseContent(
  phase: MultiplayerDraftPhase,
  onLeave: () => void,
): React.ReactNode {
  switch (phase) {
    case "idle":
    case "connecting":
      return <PodSetup />;
    case "lobby":
      return <DraftPodLobby onLeave={onLeave} />;
    case "drafting":
      return (
        <div className="flex gap-4">
          <div className="flex-1">
            <DraftProgress />
            <PackDisplay />
          </div>
          <PoolPanel />
        </div>
      );
    case "deckbuilding":
      return <LimitedDeckBuilder />;
    case "pairing":
      return (
        <div className="flex flex-col items-center justify-center gap-4 py-24">
          <div className="text-xl font-medium text-white">
            Generating pairings...
          </div>
          <p className="text-sm text-white/50">
            All decks submitted. Match pairings will appear shortly.
          </p>
        </div>
      );
    case "complete":
      return (
        <div className="flex flex-col items-center justify-center gap-4 py-24">
          <div className="text-xl font-medium text-white">Draft Complete</div>
          <button
            onClick={onLeave}
            className={menuButtonClass({ tone: "emerald", size: "md" })}
          >
            Return to Menu
          </button>
        </div>
      );
    case "error":
    case "kicked":
    case "hostLeft":
      return (
        <div className="flex flex-col items-center justify-center gap-4 py-24">
          <div className="text-xl font-medium text-red-300">
            {phase === "kicked"
              ? "You were kicked from the pod"
              : phase === "hostLeft"
                ? "Host left the draft"
                : "Connection Error"}
          </div>
          <button
            onClick={onLeave}
            className={menuButtonClass({ tone: "neutral", size: "md" })}
          >
            Return to Menu
          </button>
        </div>
      );
  }
}

// ── Page ───────────────────────────────────────────────────────────────

export function DraftPodPage() {
  const phase = useMultiplayerDraftStore((s) => s.phase);
  const leave = useMultiplayerDraftStore((s) => s.leave);
  const resetPod = useDraftPodStore((s) => s.reset);
  const navigate = useNavigate();

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      void leave();
      resetPod();
    };
  }, [leave, resetPod]);

  const handleLeave = useCallback(async () => {
    await leave();
    resetPod();
    navigate("/");
  }, [leave, resetPod, navigate]);

  const showBack = phase === "idle" || phase === "connecting";

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <ScreenChrome onBack={showBack ? () => navigate("/") : undefined} />

      <div className="relative z-10 mx-auto flex w-full max-w-6xl flex-col px-6 py-16">
        {phaseContent(phase, handleLeave)}
      </div>
    </div>
  );
}
