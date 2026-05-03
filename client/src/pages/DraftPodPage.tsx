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

import { CardPreview } from "../components/card/CardPreview";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { DraftPodLobby } from "../components/draft/DraftPodLobby";
import { DraftProgress } from "../components/draft/DraftProgress";
import { EliminationBracket } from "../components/draft/EliminationBracket";
import { HostControls } from "../components/draft/HostControls";
import { LimitedDeckBuilder } from "../components/draft/LimitedDeckBuilder";
import { PackDisplay } from "../components/draft/PackDisplay";
import { PickTimer } from "../components/draft/PickTimer";
import { PoolPanel } from "../components/draft/PoolPanel";
import { ScoreBadge } from "../components/draft/ScoreBadge";
import { SeatStatusRing } from "../components/draft/SeatStatusRing";
import { SetSelector } from "../components/draft/SetSelector";
import { StandingsTable } from "../components/draft/StandingsTable";
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

        {/* Tournament Format (D-04) */}
        <div className="flex flex-col gap-1">
          <label className="text-sm font-medium text-white/60">
            Tournament Format
          </label>
          <div className="flex gap-4">
            <label className="flex items-center gap-2 text-sm text-white/70">
              <input
                type="radio"
                name="tournamentFormat"
                checked={config.tournamentFormat === "Swiss"}
                onChange={() => setConfig({ tournamentFormat: "Swiss" })}
                className="accent-emerald-400"
              />
              Swiss (3 rounds)
            </label>
            <label className="flex items-center gap-2 text-sm text-white/70">
              <input
                type="radio"
                name="tournamentFormat"
                checked={config.tournamentFormat === "SingleElimination"}
                onChange={() =>
                  setConfig({ tournamentFormat: "SingleElimination" })
                }
                className="accent-emerald-400"
              />
              Single Elimination
            </label>
          </div>
        </div>

        {/* Pod Policy (D-07) */}
        <div className="flex flex-col gap-1">
          <label className="text-sm font-medium text-white/60">
            Pod Policy
          </label>
          <div className="flex gap-4">
            <label className="flex items-center gap-2 text-sm text-white/70">
              <input
                type="radio"
                name="podPolicy"
                checked={config.podPolicy === "Competitive"}
                onChange={() => setConfig({ podPolicy: "Competitive" })}
                className="accent-emerald-400"
              />
              Competitive
            </label>
            <label className="flex items-center gap-2 text-sm text-white/70">
              <input
                type="radio"
                name="podPolicy"
                checked={config.podPolicy === "Casual"}
                onChange={() => setConfig({ podPolicy: "Casual" })}
                className="accent-emerald-400"
              />
              Casual
            </label>
          </div>
          <p className="text-xs text-white/40">
            {config.podPolicy === "Competitive"
              ? "Timed picks, auto-pick on timeout, auto-advance rounds"
              : "Untimed picks, host controls round advancement"}
          </p>
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

// ── Phase Sub-Components ─────────────────────────────────────────────

function FormatStandings() {
  const tournamentFormat = useMultiplayerDraftStore(
    (s) => s.view?.tournament_format,
  );
  return tournamentFormat === "SingleElimination" ? (
    <EliminationBracket />
  ) : (
    <StandingsTable />
  );
}

function PairingPhaseView() {
  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 py-8">
      <h2 className="text-center text-xl font-medium text-white">
        Tournament Pairings
      </h2>
      <FormatStandings />
    </div>
  );
}

function MatchInProgressView() {
  const matchPairing = useMultiplayerDraftStore((s) => s.matchPairing);
  const [showPool, setShowPool] = useState(false);

  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 py-8">
      <h2 className="text-center text-xl font-medium text-white">
        Matches In Progress
      </h2>
      {matchPairing ? (
        <div className="rounded-xl border border-emerald-400/20 bg-emerald-400/5 p-4 text-center">
          <div className="text-sm text-white/50">Your match</div>
          <div className="text-lg text-white">
            vs {matchPairing.opponentName}
          </div>
          <div className="mt-1 text-sm text-white/40">
            {matchPairing.isMatchHost
              ? "You are hosting"
              : "Connecting to opponent..."}
          </div>
        </div>
      ) : (
        <div className="text-center text-white/50">
          Waiting for match results...
        </div>
      )}
      <FormatStandings />
      {/* D-14: ability to review own pool/deck during match phase */}
      <div className="border-t border-white/10 pt-4">
        <button
          onClick={() => setShowPool((v) => !v)}
          className="text-sm text-emerald-400 transition-colors hover:text-emerald-300"
        >
          {showPool ? "Hide Pool" : "Review Pool"}
        </button>
        {showPool && <PoolPanel />}
      </div>
    </div>
  );
}

function RoundCompleteView() {
  const podPolicy = useMultiplayerDraftStore((s) => s.view?.pod_policy);

  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 py-8">
      <h2 className="text-center text-xl font-medium text-white">
        Round Complete
      </h2>
      <FormatStandings />
      <p className="text-center text-sm text-white/50">
        {podPolicy === "Casual"
          ? "Waiting for host to start next round..."
          : "Next round starting shortly..."}
      </p>
    </div>
  );
}

// ── Between Games View (Bo3) ─────────────────────────────────────────

function BetweenGamesView() {
  const sideboardPrompt = useMultiplayerDraftStore((s) => s.sideboardPrompt);
  const playDrawPrompt = useMultiplayerDraftStore((s) => s.playDrawPrompt);
  const sideboardSubmitted = useMultiplayerDraftStore((s) => s.sideboardSubmitted);
  const seatIndex = useMultiplayerDraftStore((s) => s.seatIndex);
  const submitSideboard = useMultiplayerDraftStore((s) => s.submitSideboard);
  const choosePlayDraw = useMultiplayerDraftStore((s) => s.choosePlayDraw);
  const timerRemainingMs = useMultiplayerDraftStore((s) => s.timerRemainingMs);
  const mainDeck = useMultiplayerDraftStore((s) => s.mainDeck);
  const submittedDeck = useMultiplayerDraftStore((s) => s.submittedDeck);

  // Play/draw choice prompt (shown to the loser of the previous game)
  if (playDrawPrompt) {
    const timerSec = timerRemainingMs != null ? Math.ceil(timerRemainingMs / 1000) : null;
    return (
      <div className="mx-auto flex w-full max-w-md flex-col items-center gap-6 py-8">
        <h2 className="text-xl font-medium text-white">Game {playDrawPrompt.gameNumber}</h2>
        <ScoreBadge score={playDrawPrompt.score} player={seatIndex === 0 ? 0 : 1} size="md" />
        <p className="text-sm text-white/60">You lost the previous game. Choose:</p>
        {timerSec != null && (
          <span className="text-xs tabular-nums text-amber-300">{timerSec}s</span>
        )}
        <div className="flex gap-4">
          <button
            onClick={() => choosePlayDraw(playDrawPrompt.matchId, true)}
            className={menuButtonClass({ tone: "emerald", size: "md" })}
          >
            Play First
          </button>
          <button
            onClick={() => choosePlayDraw(playDrawPrompt.matchId, false)}
            className={menuButtonClass({ tone: "blue", size: "md" })}
          >
            Draw First
          </button>
        </div>
      </div>
    );
  }

  // Sideboard submitted — waiting for opponent
  if (sideboardSubmitted) {
    return (
      <div className="mx-auto flex w-full max-w-md flex-col items-center gap-6 py-8">
        <h2 className="text-xl font-medium text-white">Sideboarding</h2>
        {sideboardPrompt && (
          <ScoreBadge score={sideboardPrompt.score} player={seatIndex === 0 ? 0 : 1} size="md" />
        )}
        <p className="text-sm text-white/60">
          Waiting for opponent to submit sideboard...
        </p>
        <div className="h-6 w-6 animate-spin rounded-full border-2 border-white/20 border-t-emerald-400" />
      </div>
    );
  }

  // Sideboard editing (reuse deck from submitted or current mainDeck)
  if (sideboardPrompt) {
    const timerSec = timerRemainingMs != null ? Math.ceil(timerRemainingMs / 1000) : null;
    const currentDeck = submittedDeck.length > 0 ? submittedDeck : mainDeck;

    return (
      <div className="mx-auto flex w-full max-w-4xl flex-col gap-4 py-8">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <h2 className="text-xl font-medium text-white">
              Sideboard — Game {sideboardPrompt.gameNumber}
            </h2>
            <ScoreBadge score={sideboardPrompt.score} player={seatIndex === 0 ? 0 : 1} size="md" />
          </div>
          {timerSec != null && (
            <span className="text-sm tabular-nums text-amber-300">{timerSec}s remaining</span>
          )}
        </div>
        <p className="text-sm text-white/50">
          Make sideboard changes, then submit. Your pool is available below.
        </p>
        {/* Reuse the LimitedDeckBuilder for sideboard editing */}
        <LimitedDeckBuilder />
        <button
          onClick={() => {
            // Submit current deck state as sideboard submission
            submitSideboard(sideboardPrompt.matchId, currentDeck, []);
          }}
          className={menuButtonClass({ tone: "emerald", size: "md" })}
        >
          Submit Sideboard
        </button>
      </div>
    );
  }

  // Fallback — should not reach here
  return (
    <div className="mx-auto flex w-full max-w-md flex-col items-center gap-6 py-8">
      <p className="text-sm text-white/60">Preparing next game...</p>
    </div>
  );
}

function DraftingPhaseContent() {
  const [hoveredCardName, setHoveredCardName] = useState<string | null>(null);

  return (
    <>
      <div className="flex gap-4">
        <div className="flex min-w-0 flex-1 flex-col">
          <SeatStatusRing />
          <PickTimer />
          <DraftProgress />
          <PackDisplay onCardHover={setHoveredCardName} />
        </div>
        <PoolPanel onCardHover={setHoveredCardName} />
      </div>
      <CardPreview cardName={hoveredCardName} />
    </>
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
      return <DraftingPhaseContent />;
    case "deckbuilding":
      return <LimitedDeckBuilder />;
    case "betweenGames":
      return <BetweenGamesView />;
    case "pairing":
      return <PairingPhaseView />;
    case "matchInProgress":
      return <MatchInProgressView />;
    case "roundComplete":
      return <RoundCompleteView />;
    case "complete":
      return (
        <div className="mx-auto flex w-full max-w-2xl flex-col items-center gap-6 py-8">
          <div className="text-2xl font-bold text-white">Draft Complete</div>
          <FormatStandings />
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

      <HostControls />
    </div>
  );
}
