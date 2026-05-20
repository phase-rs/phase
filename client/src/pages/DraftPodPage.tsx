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
import { useNavigate, useSearchParams } from "react-router";

import { CardPreview } from "../components/card/CardPreview";
import type { CardHoverInfo } from "../components/card/CardPreview";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { DraftIntro } from "../components/draft/DraftIntro";
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
  const kindDescription = config.kind === "Premier"
    ? "Best-of-one tournament matches after deckbuilding. Faster rounds, no sideboarding between games."
    : "Best-of-three tournament matches after deckbuilding, with sideboarding between games.";
  const tournamentDescription = config.tournamentFormat === "Swiss"
    ? "Everyone keeps playing through three rounds, even after a loss."
    : "Players are eliminated after a match loss until one winner remains.";
  const policyDescription = config.podPolicy === "Competitive"
    ? "Timed picks, automatic picks on timeout, and automatic round advancement."
    : "Untimed picks, manual round advancement, and host controls for resolving issues.";
  const podSizeDescription = `${config.podSize} total seats. Empty seats can be filled with bots from the lobby before the draft starts.`;

  if (mode === "choose") {
    return (
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-8">
        <div className="flex flex-col items-center gap-2">
          <h1 className="menu-display text-3xl text-white">Pod Draft</h1>
          <p className="text-sm text-white/50">
            Draft with friends — open packs, pick cards, and play tournament matches.
          </p>
        </div>

        <div className="grid gap-4 sm:grid-cols-2">
          {/* Host card */}
          <button
            onClick={() => setMode("host")}
            className="group flex flex-col gap-3 rounded-[16px] border border-emerald-300/18 bg-emerald-400/5 p-6 text-left backdrop-blur-md transition-colors hover:border-emerald-300/30 hover:bg-emerald-400/10"
          >
            <div className="text-lg font-semibold text-emerald-100">Host a Pod</div>
            <p className="text-sm leading-relaxed text-white/50 group-hover:text-white/60">
              Create a new draft room. You choose the set, format, and pod
              size — then share the room code with your friends. Empty seats
              can be filled with bots.
            </p>
          </button>

          {/* Join card */}
          <button
            onClick={() => setMode("join")}
            className="group flex flex-col gap-3 rounded-[16px] border border-blue-300/18 bg-blue-400/5 p-6 text-left backdrop-blur-md transition-colors hover:border-blue-300/30 hover:bg-blue-400/10"
          >
            <div className="text-lg font-semibold text-blue-100">Join a Pod</div>
            <p className="text-sm leading-relaxed text-white/50 group-hover:text-white/60">
              Enter a room code from the host to join an existing draft.
              You'll be seated in the next open slot and draft alongside
              everyone in real time.
            </p>
          </button>
        </div>

        <div className="rounded-[16px] border border-white/8 bg-white/3 px-5 py-4 backdrop-blur-md">
          <div className="mb-2 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
            How Pod Draft works
          </div>
          <ul className="space-y-1.5 text-sm leading-relaxed text-white/50">
            <li>Each player opens 3 packs of 14 cards — pick one, pass the rest</li>
            <li>Packs alternate direction each round (left → right → left)</li>
            <li>After drafting, everyone builds a 40-card deck from their pool</li>
            <li>Then play Swiss or single-elimination tournament matches</li>
          </ul>
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
          <h1 className="menu-display text-3xl text-white">Host a Pod</h1>
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
        <div className="flex flex-col gap-1">
          <label className="text-sm font-medium text-white/60">
            Draft Type
          </label>
          <div className="flex gap-4">
            <label className="flex items-center gap-2 text-sm text-white/70">
              <input
                type="radio"
                name="draftKind"
                checked={config.kind === "Premier"}
                onChange={() => setConfig({ kind: "Premier" })}
                className="accent-emerald-400"
              />
              Premier (best-of-1)
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
          <p className="text-xs text-white/40">{kindDescription}</p>
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
          <p className="text-xs text-white/40">{tournamentDescription}</p>
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
          <p className="text-xs text-white/40">{policyDescription}</p>
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
          <p className="text-xs text-white/40">{podSizeDescription}</p>
        </div>

        {/* Set selector — reuse the Quick Draft component */}
        <div className="rounded-[16px] border border-white/8 bg-white/3 px-4 py-3 text-sm text-white/45">
          Choose the draft set last. Selecting a set loads its card pool and creates the pod room.
        </div>
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
        <h1 className="menu-display text-3xl text-white">Join a Pod</h1>
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
  const [hoveredCard, setHoveredCard] = useState<CardHoverInfo | null>(null);
  const [introDismissed, setIntroDismissed] = useState(false);
  const podSize = useDraftPodStore((s) => s.config.podSize);
  const view = useMultiplayerDraftStore((s) => s.view);
  const selectedCard = useMultiplayerDraftStore((s) => s.selectedCard);
  const selectCard = useMultiplayerDraftStore((s) => s.selectCard);
  const confirmPick = useMultiplayerDraftStore((s) => s.confirmPick);
  const autoPickCard = useMultiplayerDraftStore((s) => s.autoPickCard);

  if (!introDismissed) {
    return <DraftIntro mode="pod" podSize={podSize} onContinue={() => setIntroDismissed(true)} />;
  }

  return (
    <>
      <div className="flex gap-4">
        <div className="flex min-w-0 flex-1 flex-col">
          <SeatStatusRing />
          <PickTimer />
          <DraftProgress view={view} />
          <PackDisplay
            view={view}
            selectedCard={selectedCard}
            onSelectCard={selectCard}
            onConfirmPick={confirmPick}
            showAutoPick
            onAutoPick={autoPickCard}
            onCardHover={setHoveredCard}
          />
        </div>
        <PoolPanel view={view} onCardHover={setHoveredCard} />
      </div>
      <CardPreview cardName={hoveredCard?.name ?? null} sourcePrinting={hoveredCard?.sourcePrinting} />
    </>
  );
}

function PodDeckBuilder() {
  const view = useMultiplayerDraftStore((s) => s.view);
  const mainDeck = useMultiplayerDraftStore((s) => s.mainDeck);
  const landCounts = useMultiplayerDraftStore((s) => s.landCounts);
  const addToDeck = useMultiplayerDraftStore((s) => s.addToDeck);
  const removeFromDeck = useMultiplayerDraftStore((s) => s.removeFromDeck);
  const setLandCount = useMultiplayerDraftStore((s) => s.setLandCount);
  const submitDeck = useMultiplayerDraftStore((s) => s.submitDeck);

  return (
    <LimitedDeckBuilder
      view={view}
      mainDeck={mainDeck}
      landCounts={landCounts}
      onAddToDeck={addToDeck}
      onRemoveFromDeck={removeFromDeck}
      onSetLandCount={setLandCount}
      onSubmitDeck={submitDeck}
      showSuggestions={false}
    />
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
      return <PodDeckBuilder />;
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
          <h1 className="menu-display text-3xl text-white">Draft Complete</h1>
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
  const resumeHostedPod = useDraftPodStore((s) => s.resumeHostedPod);
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      void leave(true);
      resetPod();
    };
  }, [leave, resetPod]);

  useEffect(() => {
    if (searchParams.get("resume") !== "1") return;
    void resumeHostedPod();
  }, [resumeHostedPod, searchParams]);

  const handleLeave = useCallback(async () => {
    await leave(true);
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
