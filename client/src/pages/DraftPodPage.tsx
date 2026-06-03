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
import { useTranslation } from "react-i18next";
import { useNavigate, useSearchParams } from "react-router";

import { CardPreview } from "../components/card/CardPreview";
import type { CardHoverInfo } from "../components/card/CardPreview";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { CubeSetupPanel } from "../components/draft/CubeSetupPanel";
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
import { MenuShell } from "../components/menu/MenuShell";
import {
  useMultiplayerDraftStore,
  type MultiplayerDraftPhase,
} from "../stores/multiplayerDraftStore";
import { useDraftPodStore } from "../stores/draftPodStore";

// ── Setup Mode ────────────────────────────────────────────────────────

type SetupMode = "choose" | "host" | "join";

function PodSetup() {
  const { t } = useTranslation("draft");
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
  const poolMode = useDraftPodStore((s) => s.poolMode);
  const setPoolMode = useDraftPodStore((s) => s.setPoolMode);
  const setCubeForm = useDraftPodStore((s) => s.setCubeForm);
  const kindDescription = config.kind === "Premier"
    ? t("podSetup.kindPremierDesc")
    : t("podSetup.kindTraditionalDesc");
  const tournamentDescription = config.tournamentFormat === "Swiss"
    ? t("podSetup.tournamentSwissDesc")
    : t("podSetup.tournamentEliminationDesc");
  const policyDescription = config.podPolicy === "Competitive"
    ? t("podSetup.policyCompetitiveDesc")
    : t("podSetup.policyCasualDesc");
  const podSizeDescription = t("podSetup.podSizeDesc", { count: config.podSize });

  if (mode === "choose") {
    return (
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-8">
        <div className="flex flex-col items-center gap-2">
          <h1 className="menu-display text-3xl text-white">{t("podSetup.title")}</h1>
          <p className="text-sm text-white/50">
            {t("podSetup.subtitle")}
          </p>
        </div>

        <div className="grid gap-4 sm:grid-cols-2">
          {/* Host card */}
          <button
            onClick={() => setMode("host")}
            className="group flex flex-col gap-3 rounded-card border border-jade/30 surface-card p-6 text-left transition-all duration-150 hover:-translate-y-[3px] hover:border-jade/50 hover:shadow-panel"
          >
            <div className="font-display text-lg font-semibold text-jade-text">{t("podSetup.hostCardTitle")}</div>
            <p className="text-sm leading-relaxed text-fg-card-body group-hover:text-fg-muted">
              {t("podSetup.hostCardDesc")}
            </p>
          </button>

          {/* Join card */}
          <button
            onClick={() => setMode("join")}
            className="group flex flex-col gap-3 rounded-card border border-arcane/30 surface-card p-6 text-left transition-all duration-150 hover:-translate-y-[3px] hover:border-arcane/50 hover:shadow-panel"
          >
            <div className="font-display text-lg font-semibold text-arcane-text">{t("podSetup.joinCardTitle")}</div>
            <p className="text-sm leading-relaxed text-fg-card-body group-hover:text-fg-muted">
              {t("podSetup.joinCardDesc")}
            </p>
          </button>
        </div>

        <div className="rounded-card border border-hairline-strong bg-surface-panel px-5 py-4">
          <div className="mb-2 text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-fg-meta">
            {t("podSetup.howItWorksTitle")}
          </div>
          <ul className="space-y-1.5 text-sm leading-relaxed text-white/50">
            <li>{t("podSetup.howItWorks1")}</li>
            <li>{t("podSetup.howItWorks2")}</li>
            <li>{t("podSetup.howItWorks3")}</li>
            <li>{t("podSetup.howItWorks4")}</li>
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
            {t("podSetup.back")}
          </button>
          <h1 className="menu-display text-3xl text-white">{t("podSetup.hostTitle")}</h1>
        </div>

        {/* Display name */}
        <div className="flex flex-col gap-1">
          <label className="text-sm font-medium text-white/60">
            {t("podSetup.displayName")}
          </label>
          <input
            type="text"
            value={hostDisplayName}
            onChange={(e) => setHostDisplayName(e.target.value)}
            placeholder={t("podSetup.namePlaceholder")}
            className="rounded-lg border border-white/10 bg-black/30 px-4 py-2 text-white placeholder-white/30 outline-none focus:border-emerald-400/40"
          />
        </div>

        {/* Draft type */}
        <div className="flex flex-col gap-1">
          <label className="text-sm font-medium text-white/60">
            {t("podSetup.draftType")}
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
              {t("podSetup.kindPremier")}
            </label>
            <label className="flex items-center gap-2 text-sm text-white/70">
              <input
                type="radio"
                name="draftKind"
                checked={config.kind === "Traditional"}
                onChange={() => setConfig({ kind: "Traditional" })}
                className="accent-emerald-400"
              />
              {t("podSetup.kindTraditional")}
            </label>
          </div>
          <p className="text-xs text-white/40">{kindDescription}</p>
        </div>

        {/* Tournament Format (D-04) */}
        <div className="flex flex-col gap-1">
          <label className="text-sm font-medium text-white/60">
            {t("podSetup.tournamentFormat")}
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
              {t("podSetup.tournamentSwiss")}
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
              {t("podSetup.tournamentElimination")}
            </label>
          </div>
          <p className="text-xs text-white/40">{tournamentDescription}</p>
        </div>

        {/* Pod Policy (D-07) */}
        <div className="flex flex-col gap-1">
          <label className="text-sm font-medium text-white/60">
            {t("podSetup.podPolicy")}
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
              {t("podSetup.policyCompetitive")}
            </label>
            <label className="flex items-center gap-2 text-sm text-white/70">
              <input
                type="radio"
                name="podPolicy"
                checked={config.podPolicy === "Casual"}
                onChange={() => setConfig({ podPolicy: "Casual" })}
                className="accent-emerald-400"
              />
              {t("podSetup.policyCasual")}
            </label>
          </div>
          <p className="text-xs text-white/40">{policyDescription}</p>
        </div>

        {/* Pod size */}
        <div className="flex flex-col gap-1">
          <label className="text-sm font-medium text-white/60">{t("podSetup.podSize")}</label>
          <select
            value={config.podSize}
            onChange={(e) => setConfig({ podSize: Number(e.target.value) })}
            className="w-32 rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-white outline-none focus:border-emerald-400/40"
          >
            {[4, 6, 8].map((n) => (
              <option key={n} value={n}>
                {t("podSetup.playerCount", { count: n })}
              </option>
            ))}
          </select>
          <p className="text-xs text-white/40">{podSizeDescription}</p>
        </div>

        {/* Pool source: Set vs Cube tab switch */}
        <div className="flex gap-2 border-b border-white/10">
          <button
            type="button"
            onClick={() => setPoolMode("set")}
            className={
              poolMode === "set"
                ? "border-b-2 border-emerald-400 px-4 py-2 text-sm font-medium text-white"
                : "border-b-2 border-transparent px-4 py-2 text-sm text-white/50 hover:text-white/75"
            }
          >
            {t("podSetup.tabs.set")}
          </button>
          <button
            type="button"
            onClick={() => setPoolMode("cube")}
            className={
              poolMode === "cube"
                ? "border-b-2 border-emerald-400 px-4 py-2 text-sm font-medium text-white"
                : "border-b-2 border-transparent px-4 py-2 text-sm text-white/50 hover:text-white/75"
            }
          >
            {t("podSetup.tabs.cube")}
          </button>
        </div>

        {poolMode === "set" ? (
          <>
            {/* Set selector — reuse the Quick Draft component */}
            <div className="rounded-[16px] border border-white/8 bg-white/3 px-4 py-3 text-sm text-white/45">
              {t("podSetup.setSelectorHint")}
            </div>
            <SetSelector
              onStartDraft={(setCode) => {
                setConfig({ setCode });
                void createPod();
              }}
            />
          </>
        ) : (
          <CubeSetupPanel
            onStart={({ cubeName, cubeListText, settings }) => {
              setCubeForm({ cubeName, cubeListText, settings });
              void createPod();
            }}
            disabled={loadingPool}
          />
        )}

        {/* Error */}
        {configError && (
          <div className="rounded-lg border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-300">
            {configError}
          </div>
        )}

        {/* Loading */}
        {loadingPool && (
          <div className="text-sm text-white/50">{t("podSetup.loadingPool")}</div>
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
          {t("podSetup.back")}
        </button>
        <h1 className="menu-display text-3xl text-white">{t("podSetup.joinTitle")}</h1>
      </div>

      {/* Display name */}
      <div className="flex flex-col gap-1">
        <label className="text-sm font-medium text-white/60">
          {t("podSetup.displayName")}
        </label>
        <input
          type="text"
          value={guestDisplayName}
          onChange={(e) => setGuestDisplayName(e.target.value)}
          placeholder={t("podSetup.namePlaceholder")}
          className="rounded-lg border border-white/10 bg-black/30 px-4 py-2 text-white placeholder-white/30 outline-none focus:border-emerald-400/40"
        />
      </div>

      {/* Room code */}
      <div className="flex flex-col gap-1">
        <label className="text-sm font-medium text-white/60">{t("podSetup.roomCode")}</label>
        <input
          type="text"
          value={joinCode}
          onChange={(e) => setJoinCode(e.target.value.toUpperCase())}
          placeholder={t("podSetup.roomCodePlaceholder")}
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
        {t("podSetup.joinPod")}
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
  const { t } = useTranslation("draft");
  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 py-8">
      <h2 className="text-center text-xl font-medium text-white">
        {t("podPhaseView.tournamentPairings")}
      </h2>
      <FormatStandings />
    </div>
  );
}

function MatchInProgressView() {
  const { t } = useTranslation("draft");
  const navigate = useNavigate();
  const matchPairing = useMultiplayerDraftStore((s) => s.matchPairing);
  const startMatch = useMultiplayerDraftStore((s) => s.startMatch);
  const [showPool, setShowPool] = useState(false);
  const [hoveredCard, setHoveredCard] = useState<CardHoverInfo | null>(null);
  const opponentName = matchPairing
    ? matchPairing.type === "Bot"
      ? matchPairing.botName
      : matchPairing.opponentName
    : null;
  const isBotMatch = matchPairing?.type === "Bot";
  const isHost = matchPairing?.type === "HumanHost";

  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 py-8">
      <h2 className="text-center text-xl font-medium text-white">
        {t("podPhaseView.matchesInProgress")}
      </h2>
      {matchPairing ? (
        <div className="rounded-xl border border-emerald-400/20 bg-emerald-400/5 p-4 text-center">
          <div className="text-sm text-white/50">{t("podPhaseView.yourMatch")}</div>
          <div className="text-lg text-white">
            {t("podPhaseView.versusOpponent", { name: opponentName })}
          </div>
          {!isBotMatch && (
            <div className="mb-3 mt-1 text-sm text-white/40">
              {isHost
                ? t("podPhaseView.youAreHosting")
                : t("podPhaseView.connectingOpponent")}
            </div>
          )}
          <button
            onClick={() => {
              void startMatch().then((gameId) => {
                if (gameId) navigate(`/game/${gameId}?mode=draft-match`);
              });
            }}
            className={menuButtonClass({
              tone: "emerald",
              size: "sm",
              className: isBotMatch ? "mt-3" : undefined,
            })}
          >
            {t("formatPicker.startMatch")}
          </button>
        </div>
      ) : (
        <div className="text-center text-white/50">
          {t("podPhaseView.waitingResults")}
        </div>
      )}
      <FormatStandings />
      {/* D-14: ability to review own pool/deck during match phase */}
      <div className="border-t border-white/10 pt-4">
        <button
          onClick={() => setShowPool((v) => !v)}
          className="text-sm text-emerald-400 transition-colors hover:text-emerald-300"
        >
          {showPool ? t("podPhaseView.hidePool") : t("podPhaseView.reviewPool")}
        </button>
        {showPool && <PoolPanel onCardHover={setHoveredCard} />}
      </div>
      <CardPreview
        cardName={hoveredCard?.name ?? null}
        sourcePrinting={hoveredCard?.sourcePrinting}
        mobileLayout="compact"
        onDismiss={() => setHoveredCard(null)}
      />
    </div>
  );
}

function RoundCompleteView() {
  const { t } = useTranslation("draft");
  const podPolicy = useMultiplayerDraftStore((s) => s.view?.pod_policy);

  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 py-8">
      <h2 className="text-center text-xl font-medium text-white">
        {t("podPhaseView.roundComplete")}
      </h2>
      <FormatStandings />
      <p className="text-center text-sm text-white/50">
        {podPolicy === "Casual"
          ? t("podPhaseView.waitingNextRound")
          : t("podPhaseView.nextRoundShortly")}
      </p>
    </div>
  );
}

// ── Between Games View (Bo3) ─────────────────────────────────────────

function BetweenGamesView() {
  const { t } = useTranslation("draft");
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
        <h2 className="text-xl font-medium text-white">{t("betweenGames.game", { number: playDrawPrompt.gameNumber })}</h2>
        <ScoreBadge score={playDrawPrompt.score} player={seatIndex === 0 ? 0 : 1} size="md" />
        <p className="text-sm text-white/60">{t("betweenGames.lostPreviousGame")}</p>
        {timerSec != null && (
          <span className="text-xs tabular-nums text-amber-300">{t("betweenGames.seconds", { count: timerSec })}</span>
        )}
        <div className="flex gap-4">
          <button
            onClick={() => choosePlayDraw(playDrawPrompt.matchId, true)}
            className={menuButtonClass({ tone: "emerald", size: "md" })}
          >
            {t("betweenGames.playFirst")}
          </button>
          <button
            onClick={() => choosePlayDraw(playDrawPrompt.matchId, false)}
            className={menuButtonClass({ tone: "blue", size: "md" })}
          >
            {t("betweenGames.drawFirst")}
          </button>
        </div>
      </div>
    );
  }

  // Sideboard submitted — waiting for opponent
  if (sideboardSubmitted) {
    return (
      <div className="mx-auto flex w-full max-w-md flex-col items-center gap-6 py-8">
        <h2 className="text-xl font-medium text-white">{t("betweenGames.sideboarding")}</h2>
        {sideboardPrompt && (
          <ScoreBadge score={sideboardPrompt.score} player={seatIndex === 0 ? 0 : 1} size="md" />
        )}
        <p className="text-sm text-white/60">
          {t("betweenGames.waitingSideboard")}
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
              {t("betweenGames.sideboardGame", { number: sideboardPrompt.gameNumber })}
            </h2>
            <ScoreBadge score={sideboardPrompt.score} player={seatIndex === 0 ? 0 : 1} size="md" />
          </div>
          {timerSec != null && (
            <span className="text-sm tabular-nums text-amber-300">{t("betweenGames.secondsRemaining", { count: timerSec })}</span>
          )}
        </div>
        <p className="text-sm text-white/50">
          {t("betweenGames.sideboardHint")}
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
          {t("betweenGames.submitSideboard")}
        </button>
      </div>
    );
  }

  // Fallback — should not reach here
  return (
    <div className="mx-auto flex w-full max-w-md flex-col items-center gap-6 py-8">
      <p className="text-sm text-white/60">{t("betweenGames.preparingNext")}</p>
    </div>
  );
}

function DraftingPhaseContent() {
  const { t } = useTranslation("draft");
  const [hoveredCard, setHoveredCard] = useState<CardHoverInfo | null>(null);
  const [introDismissed, setIntroDismissed] = useState(false);
  const podSize = useDraftPodStore((s) => s.config.podSize);
  const view = useMultiplayerDraftStore((s) => s.view);
  const selectedCard = useMultiplayerDraftStore((s) => s.selectedCard);
  const selectCard = useMultiplayerDraftStore((s) => s.selectCard);
  const confirmPick = useMultiplayerDraftStore((s) => s.confirmPick);
  const autoPickCard = useMultiplayerDraftStore((s) => s.autoPickCard);
  const paused = useMultiplayerDraftStore((s) => s.paused);
  const pauseReason = useMultiplayerDraftStore((s) => s.pauseReason);

  if (!introDismissed) {
    return <DraftIntro mode="pod" podSize={podSize} onContinue={() => setIntroDismissed(true)} />;
  }

  // Wire `pauseReason` is `DraftPauseReason` (PascalCase) — same shape as the
  // i18n key path, so no boundary conversion. Falls back to a generic key if
  // the engine ever emits an unknown reason (defensive only).
  const pauseKey = pauseReason ?? "PausedByHost";

  return (
    <>
      {paused && (
        <div
          role="status"
          className="mb-3 rounded-lg border border-amber-400/40 bg-amber-500/10 px-4 py-3 text-sm text-amber-100"
        >
          ⚠ {t(`podPhaseView.pauseReason.${pauseKey}`)}
        </div>
      )}
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

function CompleteView({ onLeave }: { onLeave: () => void }) {
  const { t } = useTranslation("draft");
  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col items-center gap-6 py-8">
      <h1 className="menu-display text-3xl text-white">{t("podComplete.title")}</h1>
      <FormatStandings />
      <button
        onClick={onLeave}
        className={menuButtonClass({ tone: "emerald", size: "md" })}
      >
        {t("podComplete.returnToMenu")}
      </button>
    </div>
  );
}

function PodErrorView({
  phase,
  onLeave,
}: {
  phase: "error" | "kicked" | "hostLeft";
  onLeave: () => void;
}) {
  const { t } = useTranslation("draft");
  const message =
    phase === "kicked"
      ? t("podError.kicked")
      : phase === "hostLeft"
        ? t("podError.hostLeft")
        : t("podError.connection");
  return (
    <div className="flex flex-col items-center justify-center gap-4 py-24">
      <div className="text-xl font-medium text-red-300">{message}</div>
      <button
        onClick={onLeave}
        className={menuButtonClass({ tone: "neutral", size: "md" })}
      >
        {t("podComplete.returnToMenu")}
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
      return <CompleteView onLeave={onLeave} />;
    case "error":
    case "kicked":
    case "hostLeft":
      return <PodErrorView phase={phase} onLeave={onLeave} />;
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
      <ScreenChrome onBack={showBack ? handleLeave : undefined} />

      {/* Centered MenuShell column — same responsive framing as every other
          out-of-match surface. Each phase view renders its own heading, so no
          MenuShell title is passed. */}
      <MenuShell layout="stacked">
        <div className="flex w-full flex-col">{phaseContent(phase, handleLeave)}</div>
      </MenuShell>

      <HostControls />
    </div>
  );
}
