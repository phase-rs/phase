/**
 * Draft Pod Page — P2P multiplayer draft flow.
 *
 * Progressive flow:
 * 1. Setup: host creates or guest joins a pod
 * 2. Lobby: 8-seat grid with bot-fill controls (DraftPodLobby)
 * 3. Drafting: pack display + pool panel (reuses Quick Draft components)
 * 4. Deckbuilding: LimitedDeckBuilder (reuses Quick Draft component)
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLocation, useNavigate, useSearchParams } from "react-router";

import { MenuSelect } from "../components/ui/MenuSelect";
import type { CardHoverInfo } from "../components/card/CardPreview";
import { HoverCardPreview } from "../components/card/HoverCardPreview";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import {
  useDraftShellChrome,
  type DraftShellPhoneAction,
  type DraftShellTopAction,
} from "../components/chrome/ShellContext";
import { usePreferencesStore } from "../stores/preferencesStore";
import { CubeSetupPanel } from "../components/draft/CubeSetupPanel";
import { DraftIntro } from "../components/draft/DraftIntro";
import { DraftPodLobby } from "../components/draft/DraftPodLobby";
import { DraftProgress } from "../components/draft/DraftProgress";
import { EliminationBracket } from "../components/draft/EliminationBracket";
import { HostControls, useHostDraftTopActions } from "../components/draft/HostControls";
import { LimitedDeckBuilder } from "../components/draft/LimitedDeckBuilder";
import { PackDisplay, type PackDisplayController } from "../components/draft/PackDisplay";
import { PickTimer } from "../components/draft/PickTimer";
import { COMMANDER_DRAFT_ENTRY, type DraftKind } from "../components/draft/draftKind";
import { distinctJoined } from "../adapter/draft-adapter";
import { PodIcon } from "../components/draft/PodIcon";
import { PoolPanel } from "../components/draft/PoolPanel";
import { ScoreBadge } from "../components/draft/ScoreBadge";
import { SeatStatusRing } from "../components/draft/SeatStatusRing";
import { SetSelector } from "../components/draft/SetSelector";
import { StandingsTable } from "../components/draft/StandingsTable";
import { PodErrorBanner } from "../components/draft/PodErrorBanner";
import {
  getResponsiveDraftLayout,
  loadDraftWorkspacePreferences,
  repairDraftWorkspacePackScale,
  saveDraftWorkspacePreferences,
  type DraftWorkspacePreferences,
  type ResponsiveDraftLayout,
} from "../components/draft/workspace/workspacePreferences";
import { DraftWorkspace } from "../components/draft/workspace/DraftWorkspace";
import { resolveWorkspacePickPlacement } from "../components/draft/workspace/workspacePlacement";
import {
  useDraftWorkspaceDrag,
  type DraftDropRequest,
  type DraftPickInteractionSnapshot,
} from "../components/draft/workspace/useDraftWorkspaceDrag";
import {
  countProjectedNames,
  projectWorkspacePartition,
} from "../components/draft/workspace/workspaceProjection";
import { DialogShell } from "../components/modal/DialogShell";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { MenuPanel, MenuShell } from "../components/menu/MenuShell";
import {
  draftPodScreen,
  DRAFT_OFFLINE_ERROR,
  intergamePromptKey,
  isMultiplayerDraftPodLive,
  useMultiplayerDraftStore,
  type DraftPodScreen,
  type GuestDraftResumeOutcome,
} from "../stores/multiplayerDraftStore";
import type { DraftPickDestination, DraftPickPlacementHint } from "../stores/draftStore";
import { useDraftPodStore } from "../stores/draftPodStore";
import { useEffectiveOffline } from "../stores/connectivityStore";

// ── Setup Mode ────────────────────────────────────────────────────────

type SetupMode = "choose" | "host" | "join";

function readPickInteraction(): DraftPickInteractionSnapshot {
  const state = useMultiplayerDraftStore.getState();
  return {
    interactionGeneration: state.interactionGeneration,
    pickInteractionLocked: state.pickInteractionLocked,
    pendingPickIntent: state.pendingPickIntent,
  };
}

function subscribePickInteraction(listener: () => void): () => void {
  return useMultiplayerDraftStore.subscribe((state, previous) => {
    if (
      state.interactionGeneration !== previous.interactionGeneration
      || state.pickInteractionLocked !== previous.pickInteractionLocked
      || state.pendingPickIntent !== previous.pendingPickIntent
    ) listener();
  });
}

function PodSetup() {
  const { t } = useTranslation("draft");
  const effectiveOffline = useEffectiveOffline();
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
  const setDraftMode = useDraftPodStore((s) => s.setDraftMode);
  const setSetDraftMode = useDraftPodStore((s) => s.setSetDraftMode);
  const setCubeForm = useDraftPodStore((s) => s.setCubeForm);
  const allowedPodSizes = useDraftPodStore((s) =>
    s.procedureCacheKey?.kind === s.config.kind
    && s.procedureCacheKey.tournamentFormat === s.config.tournamentFormat
      ? s.allowedPodSizes
      : null,
  );
  const packDistribution = useDraftPodStore((s) => s.packDistribution);
  const packsPerPlayer = useDraftPodStore((s) => s.packsPerPlayer);
  const refreshProcedure = useDraftPodStore((s) => s.refreshProcedure);

  // The kind radios record intent (`setConfig`) but publish nothing, so the
  // ENGINE's per-kind axes — booster count and allowed seat set — are re-read here
  // whenever the selected kind changes. Without this the set selector would
  // have no booster count to build a pack list against on the default entry,
  // which reaches this page with no `?kind=` deep link to load one.
  useEffect(() => {
    if (effectiveOffline) return;
    void refreshProcedure();
  }, [effectiveOffline, refreshProcedure, config.kind, config.tournamentFormat]);
  // Total over `DraftKind`: a future kind is a TS2741 at this literal rather than a
  // blank line under the radios. Values are already-resolved strings because
  // `react-i18next.d.ts` types `t`'s key against the `en` catalog, so a `t(variable)`
  // lookup would not typecheck.
  const kindDescription: Record<Exclude<DraftKind, "Quick">, string> = {
    Premier: t("podSetup.kindPremierDesc"),
    Traditional: t("podSetup.kindTraditionalDesc"),
    Sealed: t("podSetup.kindSealedDesc"),
    CommanderDraft: t("podSetup.kindCommanderDraftDesc"),
  };
  const tournamentDescription = config.tournamentFormat === "Swiss"
    ? t("podSetup.tournamentSwissDesc")
    : t("podSetup.tournamentEliminationDesc");
  const policyDescription = config.podPolicy === "Competitive"
    ? t("podSetup.policyCompetitiveDesc")
    : t("podSetup.policyCasualDesc");
  const podSizeDescription = t("podSetup.podSizeDesc", { count: config.podSize });
  const podSizeItems = useMemo(
    () => (allowedPodSizes ?? []).map((podSize) => ({
      value: String(podSize),
      label: t("podSetup.playerCount", { count: podSize }),
    })),
    [allowedPodSizes, t],
  );
  const podSizeLabel =
    podSizeItems.find((item) => item.value === String(config.podSize))?.label ??
    t("podSetup.playerCount", { count: config.podSize });
  const configErrorMessage = configError === DRAFT_OFFLINE_ERROR
    ? t("offline.startUnavailable")
    : configError;

  if (effectiveOffline) {
    return (
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-6">
        {mode !== "choose" && (
          <button
            onClick={() => setMode("choose")}
            className="w-fit text-sm text-white/50 hover:text-white/80"
          >
            {t("podSetup.back")}
          </button>
        )}
        <div className="rounded-card border border-amber-400/20 bg-amber-400/5 px-5 py-4 text-sm text-amber-100">
          {t("offline.unavailableDescription")}
        </div>
        {configErrorMessage && (
          <div className="rounded-lg border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-300">
            {configErrorMessage}
          </div>
        )}
      </div>
    );
  }

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
            <label className="flex min-h-11 items-center gap-2 py-2 text-sm text-white/70">
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
            <label className="flex items-center gap-2 text-sm text-white/70">
              <input
                type="radio"
                name="draftKind"
                checked={config.kind === "Sealed"}
                onChange={() => setConfig({ kind: "Sealed" })}
                className="accent-emerald-400"
              />
              {t("podSetup.kindSealed")}
            </label>
            <label className="flex items-center gap-2 text-sm text-white/70">
              <input
                type="radio"
                name="draftKind"
                checked={config.kind === "CommanderDraft"}
                onChange={() => setConfig({ kind: "CommanderDraft" })}
                className="accent-emerald-400"
              />
              {t("podSetup.kindCommanderDraft")}
            </label>
          </div>
          <p className="text-xs text-white/40">{kindDescription[config.kind]}</p>
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

        <div className="flex flex-col gap-1">
          <span className="text-sm font-medium text-white/60">{t("podSetup.podSize")}</span>
          <MenuSelect
            ariaLabel={t("podSetup.podSize")}
            label={podSizeLabel}
            selectedValue={String(config.podSize)}
            items={podSizeItems}
            disabled={allowedPodSizes === null}
            onSelect={(value) => setConfig({ podSize: Number(value) })}
            menuLayout="dropdown"
            fitContainer
            wrapperClassName="w-full max-w-[8rem]"
            className="min-h-[44px] !rounded-lg border border-white/10 !bg-black/30 px-3 !py-2 text-base text-white shadow-none !hover:bg-black/30 !focus-visible:ring-emerald-400/50"
          />
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
            disabled={packDistribution === "AllAtOnce"}
            className={
              poolMode === "cube"
                ? "border-b-2 border-emerald-400 px-4 py-2 text-sm font-medium text-white"
                : "border-b-2 border-transparent px-4 py-2 text-sm text-white/50 hover:text-white/75"
            }
          >
            {t("podSetup.tabs.cube")}
          </button>
        </div>

        {poolMode === "set" || packDistribution === "AllAtOnce" ? (
          <>
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/60">{t("podSetup.setDraftMode")}</span>
              <div className="flex gap-4">
                <label className="flex items-center gap-2 text-sm text-white/70">
                  <input
                    type="radio"
                    name="setDraftMode"
                    checked={setDraftMode === "uniform"}
                    onChange={() => setSetDraftMode("uniform")}
                    className="accent-emerald-400"
                  />
                  {t("podSetup.uniformPacks")}
                </label>
                <label className="flex items-center gap-2 text-sm text-white/70">
                  <input
                    type="radio"
                    name="setDraftMode"
                    checked={setDraftMode === "chaos"}
                    onChange={() => setSetDraftMode("chaos")}
                    className="accent-emerald-400"
                  />
                  {t("podSetup.chaosPacks")}
                </label>
              </div>
              <p className="text-xs text-white/40">
                {setDraftMode === "chaos"
                  ? t("podSetup.chaosSelectorHint")
                  : t("podSetup.setSelectorHint")}
              </p>
            </div>
            <div className="rounded-[16px] border border-white/8 bg-white/3 px-4 py-3 text-sm text-white/45">
              {setDraftMode === "chaos"
                ? t("podSetup.chaosSelectorDetail")
                : t("podSetup.setSelectorHint")}
            </div>
            {/* A Uniform pod carries a pack-ordered SEQUENCE to the host, so the host
                arranges one set per booster exactly as a local draft does. Chaos
                reuses this selector as a distinct candidate-set chooser; draft-wasm
                privately resolves the seat-by-round assignments from the host seed.
                `packsPerPlayer` is the ENGINE's per-kind booster count, so a
                Uniform Sealed pod asks for six and a Uniform draft pod for three
                without this page knowing either number; until it loads the list is
                locked at zero rather than guessing one. Deliberately NOT
                `fixedPackCount`: naming one set still fills every booster (a
                short sequence repeats its last entry), so the old one-click
                single-set pod survives alongside the arranged one. */}
            {packsPerPlayer === null ? (
              <div className="text-sm text-white/50">{t("podSetup.loadingPool")}</div>
            ) : (
              <SetSelector
                defaultPackCount={packsPerPlayer}
                startLabel={t("podSetup.createPod")}
                candidatePool={setDraftMode === "chaos"}
                onStartDraft={(packs) => {
                  if (packs.length === 0) return;
                  setConfig({
                    packs,
                    setCode: distinctJoined(packs.map((pack) => pack.code), "+"),
                    setName: distinctJoined(packs.map((pack) => pack.name), " · "),
                  });
                  void createPod();
                }}
              />
            )}
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
        {configErrorMessage && (
          <div className="rounded-lg border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-300">
            {configErrorMessage}
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
      {configErrorMessage && (
        <div className="rounded-lg border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-300">
          {configErrorMessage}
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
      <PodErrorBanner />
      <h2 className="text-center text-xl font-medium text-white">
        {t("podPhaseView.tournamentPairings")}
      </h2>
      <FormatStandings />
    </div>
  );
}

function MatchInProgressView() {
  const { t } = useTranslation("draft");
  const draftCardPreviewMode = usePreferencesStore((s) => s.draftCardPreviewMode);
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
      <PodErrorBanner />
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
      <HoverCardPreview
        card={hoveredCard}
        mode={draftCardPreviewMode}
        hoverDelayMs={0}
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
      <PodErrorBanner />
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

function BetweenGamesView({
  responsiveLayout,
  onDismiss,
}: {
  responsiveLayout: ResponsiveDraftLayout;
  onDismiss: () => void;
}) {
  const { t } = useTranslation("draft");
  const sideboardPrompt = useMultiplayerDraftStore((s) => s.sideboardPrompt);
  const playDrawPrompt = useMultiplayerDraftStore((s) => s.playDrawPrompt);
  const sideboardSubmitted = useMultiplayerDraftStore((s) => s.sideboardSubmitted);
  const seatIndex = useMultiplayerDraftStore((s) => s.seatIndex);
  const submitSideboard = useMultiplayerDraftStore((s) => s.submitSideboard);
  const setIntergameWorkspaceState = useMultiplayerDraftStore((s) => s.setIntergameWorkspaceState);
  const choosePlayDraw = useMultiplayerDraftStore((s) => s.choosePlayDraw);
  const timerRemainingMs = useMultiplayerDraftStore((s) => s.timerRemainingMs);
  const submittedDeck = useMultiplayerDraftStore((s) => s.submittedDeck);
  const view = useMultiplayerDraftStore((s) => s.view);
  const intergameWorkspace = useMultiplayerDraftStore((s) => s.intergameWorkspaceState);
  const tabletLayout = responsiveLayout === "tablet-portrait" || responsiveLayout === "tablet-landscape";
  const [workspacePreferences, setWorkspacePreferences] = useState<DraftWorkspacePreferences>(loadDraftWorkspacePreferences);
  const handlePreferencesChange = useCallback((next: DraftWorkspacePreferences) => {
    setWorkspacePreferences(next);
    saveDraftWorkspacePreferences(next);
  }, []);

  // Play/draw choice prompt (shown to the loser of the previous game)
  if (playDrawPrompt) {
    const timerSec = timerRemainingMs != null ? Math.ceil(timerRemainingMs / 1000) : null;
    return (
      <div className="mx-auto flex w-full max-w-md flex-col items-center gap-6 py-8">
        <PodErrorBanner />
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
        <button onClick={onDismiss} className={menuButtonClass({ tone: "neutral", size: "xs" })}>
          {t("betweenGames.hideOverlay")}
        </button>
      </div>
    );
  }

  // Sideboard submitted — waiting for opponent
  if (sideboardSubmitted) {
    return (
      <div className="mx-auto flex w-full max-w-md flex-col items-center gap-6 py-8">
        <PodErrorBanner />
        <h2 className="text-xl font-medium text-white">{t("betweenGames.sideboarding")}</h2>
        {sideboardPrompt && (
          <ScoreBadge score={sideboardPrompt.score} player={seatIndex === 0 ? 0 : 1} size="md" />
        )}
        <p className="text-sm text-white/60">
          {t("betweenGames.waitingSideboard")}
        </p>
        {submittedDeck.length > 0 && (
          <p className="text-sm text-white/50">
            {submittedDeck.join(", ")}
          </p>
        )}
        <div className="h-6 w-6 animate-spin rounded-full border-2 border-white/20 border-t-emerald-400" />
        <button onClick={onDismiss} className={menuButtonClass({ tone: "neutral", size: "xs" })}>
          {t("betweenGames.hideOverlay")}
        </button>
      </div>
    );
  }

  // Sideboard editing (reuse deck from submitted or current mainDeck)
  if (sideboardPrompt && view && intergameWorkspace) {
    const timerSec = timerRemainingMs != null ? Math.ceil(timerRemainingMs / 1000) : null;

    return (
      <div className={tabletLayout
        ? "mx-auto flex h-[calc(100dvh_-_4rem)] min-h-0 w-full max-w-none flex-col gap-4 overflow-hidden"
        : "mx-auto flex w-full max-w-4xl flex-col gap-4 py-8"}
      >
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
        <div className={tabletLayout ? "min-h-0 flex-1 overflow-hidden" : undefined}>
          <LimitedDeckBuilder
            local={{
              view,
              workspace: intergameWorkspace,
              preferences: workspacePreferences,
              interactionLocked: false,
              capabilities: { kind: "fixed-pool" },
              onWorkspaceChange: setIntergameWorkspaceState,
              onPreferencesChange: handlePreferencesChange,
              onSubmitDeck: () => {
                const partition = projectWorkspacePartition(intergameWorkspace, view.pool);
                submitSideboard(
                  sideboardPrompt.matchId,
                  partition.mainDeck,
                  countProjectedNames(partition.sideboard),
                );
              },
            }}
            showSuggestions={false}
            responsiveLayout={responsiveLayout}
            responsiveHeightMode={tabletLayout ? "container" : "viewport"}
          />
        </div>
      </div>
    );
  }

  // Fallback — unreachable BY CONSTRUCTION, and retained only for the function's
  // totality: `draftPodScreen` answers `"betweenGames"` only when
  // `sideboardPrompt !== null`, and the `sideboardSubmitted` branch above fires
  // before this one. It keeps the banner and the dismiss control so it cannot
  // become a dead end if the rule ever widens.
  return (
    <div className="mx-auto flex w-full max-w-md flex-col items-center gap-6 py-8">
      <PodErrorBanner />
      <p className="text-sm text-white/60">{t("betweenGames.preparingNext")}</p>
        <button onClick={onDismiss} className={menuButtonClass({ tone: "neutral", size: "xs" })}>
          {t("betweenGames.hideOverlay")}
        </button>
    </div>
  );
}

function DraftingPhaseContent({
  responsiveLayout,
  phoneLayout,
  mobileWorkspaceOpen,
  setMobileWorkspaceOpen,
}: {
  responsiveLayout: ResponsiveDraftLayout;
  phoneLayout: boolean;
  mobileWorkspaceOpen: boolean;
  setMobileWorkspaceOpen: (open: boolean) => void;
}) {
  const { t } = useTranslation("draft");
  const draftCardPreviewMode = usePreferencesStore((s) => s.draftCardPreviewMode);
  const draftDoubleClickConfirmPick = usePreferencesStore((s) => s.draftDoubleClickConfirmPick);
  const [hoveredCard, setHoveredCard] = useState<CardHoverInfo | null>(null);
  const [introDismissed, setIntroDismissed] = useState(false);
  const [workspacePreferences, setWorkspacePreferences] = useState<DraftWorkspacePreferences>(loadDraftWorkspacePreferences);
  const view = useMultiplayerDraftStore((s) => s.view);
  const selectedCard = useMultiplayerDraftStore((s) => s.selectedCard);
  const workspaceState = useMultiplayerDraftStore((s) => s.workspaceState);
  const pendingPickIntent = useMultiplayerDraftStore((s) => s.pendingPickIntent);
  const interactionGeneration = useMultiplayerDraftStore((s) => s.interactionGeneration);
  const pickInteractionLocked = useMultiplayerDraftStore((s) => s.pickInteractionLocked);
  const selectCard = useMultiplayerDraftStore((s) => s.selectCard);
  const paused = useMultiplayerDraftStore((s) => s.paused);
  const pauseReason = useMultiplayerDraftStore((s) => s.pauseReason);
  const phoneToolbarPinned = phoneLayout && !mobileWorkspaceOpen;
  const handlePreferencesChange = useCallback((next: DraftWorkspacePreferences) => {
    if (useMultiplayerDraftStore.getState().pickInteractionLocked) return;
    setWorkspacePreferences(next);
    saveDraftWorkspacePreferences(next);
  }, []);
  const setPackScale = useCallback((next: number) => {
    setWorkspacePreferences((current) => {
      const updated = { ...current, packScale: repairDraftWorkspacePackScale(next) };
      saveDraftWorkspacePreferences(updated);
      return updated;
    });
  }, []);
  const handleDrop = useCallback((request: DraftDropRequest) => {
    const state = useMultiplayerDraftStore.getState();
    const outcome = request.source.kind === "pick"
      ? state.submitPick(request.source.instanceIds[0], request.destination, request.placementHint)
      : state.submitPickWithDraftEffect(
        request.source.authorityId,
        request.source.instanceIds,
        request.destination,
        request.placementHint,
      );
    return {
      requestToken: request.requestToken,
      interactionGeneration: request.interactionGeneration,
      outcome,
    };
  }, []);
  const resolveCollapsedSideboardColumn = useCallback((sourceInstanceId: string) => {
    const placement = useMultiplayerDraftStore.getState().workspaceState?.placements[sourceInstanceId];
    return Math.min(
      workspacePreferences.sideboard.columnCount - 1,
      Math.max(0, placement?.column ?? 0),
    );
  }, [workspacePreferences.sideboard.columnCount]);
  const interactionLocked = paused || pickInteractionLocked;
  const dragController = useDraftWorkspaceDrag({
    enabled: introDismissed && !interactionLocked,
    readPickInteraction,
    subscribePickInteraction,
    onDrop: handleDrop,
    resolveCollapsedSideboardColumn,
  });
  const handleConfirmPick = useCallback((
    destination: DraftPickDestination,
    placementHint?: DraftPickPlacementHint,
  ) => {
    const state = useMultiplayerDraftStore.getState();
    if (placementHint !== undefined || destination !== "deck") {
      return state.confirmPick(destination, placementHint);
    }
    const card = state.view?.current_pack?.find((entry) => entry.instance_id === state.selectedCard);
    const resolvedPlacement = card !== undefined && state.view !== null && state.workspaceState !== null
      ? resolveWorkspacePickPlacement(
        card,
        destination,
        state.view.pool,
        state.view.pool_groups,
        state.workspaceState,
        workspacePreferences.deck,
      )
      : { column: 0 };
    return state.confirmPick(destination, resolvedPlacement);
  }, [workspacePreferences.deck]);
  const handleAutoPick = useCallback(() => {
    const state = useMultiplayerDraftStore.getState();
    const { view: currentView, workspaceState } = state;
    const placementHints = currentView !== null && currentView.current_pack !== null && workspaceState !== null
      ? Object.fromEntries(currentView.current_pack.map((card) => [
        card.instance_id,
        resolveWorkspacePickPlacement(
          card,
          "deck",
          currentView.pool,
          currentView.pool_groups,
          workspaceState,
          workspacePreferences.deck,
        ),
      ]))
      : undefined;
    return state.autoPickCard(placementHints);
  }, [workspacePreferences.deck]);
  const packController = useMemo<PackDisplayController>(() => ({
    kind: "local-workspace",
    view,
    selectedCard,
    pendingIntent: pendingPickIntent,
    interactionGeneration,
    interactionLocked,
    doubleClickPick: draftDoubleClickConfirmPick,
    dragController,
    selectCard,
    pickCard: (instanceId, destination, placementHint) => useMultiplayerDraftStore.getState().submitPick(instanceId, destination, placementHint),
    pickCardStep: (instanceIds, destination, placementHint) => useMultiplayerDraftStore.getState().submitPickStep(instanceIds, destination, placementHint),
    confirmPick: handleConfirmPick,
    pickCardWithDraftEffect: (effectInstanceId, instanceIds, destination, placementHint) => useMultiplayerDraftStore.getState().submitPickWithDraftEffect(effectInstanceId, instanceIds, destination, placementHint),
    autoPickCard: handleAutoPick,
  }), [
    dragController, handleAutoPick, handleConfirmPick, interactionGeneration, interactionLocked, pendingPickIntent,
    selectCard, selectedCard, view, draftDoubleClickConfirmPick,
  ]);

  if (!introDismissed) {
    // The engine procedure authorizes the Commander variant, and its seat list
    // supplies the player count. Both come from the view, never from
    // `draftPodStore.config` — a guest's local config is never populated from
    // the host's pod, so it still holds this client's own defaults. Reading a
    // kind label or the count from that config would render the wrong intro.
    //
    // `phase` can reach "drafting" from a `statusChanged` event that carries no view,
    // so `view` is genuinely nullable here. In that window the seat count is unknown
    // and nothing is rendered: an intro sentence stating a confident wrong number is
    // worse than a frame with no intro, and the following `viewUpdated` supplies it.
    if (!view) return null;

    return (
      <DraftIntro
        mode={view.launch_capability === "CommanderMultiplayer" ? "commander" : "pod"}
        podSize={view.seats.length}
        packCount={view.pack_count}
        cardsPerPack={view.cards_per_pack}
        packSizes={view.pack_sizes}
        minDeckSize={view.min_deck_size}
        onContinue={() => setIntroDismissed(true)}
      />
    );
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
      <div
        data-responsive-draft-layout={responsiveLayout}
        className={responsiveLayout === "desktop"
          ? "flex w-full min-w-0 flex-col gap-4"
          : responsiveLayout === "phone-portrait"
            ? "relative flex h-[calc(100dvh_-_11rem)] min-h-0 w-full min-w-0 flex-col"
            : responsiveLayout === "phone-landscape"
              ? "relative block h-[calc(100dvh_-_4rem)] w-full min-w-0 overflow-hidden pb-[58px]"
              : responsiveLayout === "tablet-portrait"
                ? "grid h-[calc(100dvh_-_8rem)] w-full min-w-0 grid-rows-[minmax(0,56%)_minmax(0,44%)] gap-2"
                : "grid h-[calc(100dvh_-_8rem)] w-full min-w-0 grid-cols-[minmax(340px,40%)_minmax(0,60%)] gap-2"}
      >
        <div className={responsiveLayout === "desktop"
          ? "w-full min-w-0"
          : "h-full min-h-0 w-full min-w-0 overflow-hidden"}>
          {responsiveLayout === "desktop" && <SeatStatusRing />}
          {responsiveLayout === "desktop" && <DraftProgress view={view} />}
          <PickTimer />
          <PackDisplay
            controller={packController}
            presentation={{ packScale: workspacePreferences.packScale, setPackScale }}
            enableDraftEffects
            onCardHover={setHoveredCard}
            responsiveLayout={responsiveLayout}
            phoneToolbarPinned={phoneToolbarPinned}
            mobileWorkspaceOpen={mobileWorkspaceOpen}
          />
        </div>
        {view && workspaceState && (
          <div className={responsiveLayout === "desktop"
            ? "w-full min-w-0"
            : phoneLayout
              ? "h-0 min-h-0 w-full min-w-0"
              : "h-full min-h-0 w-full min-w-0"}>
            <DraftWorkspace
              pool={view.pool}
              poolGroups={view.pool_groups}
              workspace={workspaceState}
              preferences={workspacePreferences}
              interactionLocked={interactionLocked}
              dragController={dragController}
              onWorkspaceChange={(next) => useMultiplayerDraftStore.getState().setWorkspaceState(next)}
              onPreferencesChange={handlePreferencesChange}
              onCardHover={setHoveredCard}
              responsiveLayout={responsiveLayout}
              mobileOverlay
              mobileWorkspaceOpen={mobileWorkspaceOpen}
              onMobileWorkspaceOpenChange={setMobileWorkspaceOpen}
            />
          </div>
        )}
      </div>
      <HoverCardPreview
        card={hoveredCard}
        mode={draftCardPreviewMode}
        hoverDelayMs={0}
      />
    </>
  );
}

function PodDeckBuilder({ responsiveLayout }: { responsiveLayout: ResponsiveDraftLayout }) {
  const view = useMultiplayerDraftStore((s) => s.view);
  const workspace = useMultiplayerDraftStore((s) => s.workspaceState);
  const interactionLocked = useMultiplayerDraftStore((s) => s.pickInteractionLocked);
  const submitDeck = useMultiplayerDraftStore((s) => s.submitDeck);
  const submissionError = useMultiplayerDraftStore((s) => s.error);
  const [preferences, setPreferences] = useState<DraftWorkspacePreferences>(loadDraftWorkspacePreferences);
  const handlePreferencesChange = useCallback((next: DraftWorkspacePreferences) => {
    setPreferences(next);
    saveDraftWorkspacePreferences(next);
  }, []);

  if (!view || !workspace) return null;

  return (
    <LimitedDeckBuilder
      local={{
        view,
        workspace,
        preferences,
        interactionLocked,
        capabilities: { kind: "editable-pool", suggestions: false },
        onWorkspaceChange: (next) => useMultiplayerDraftStore.getState().setWorkspaceState(next),
        onPreferencesChange: handlePreferencesChange,
        onAddBasicLand: (name) => useMultiplayerDraftStore.getState().addBasicLand(name),
        onRemoveBasicLand: (name) => useMultiplayerDraftStore.getState().removeBasicLand(name),
        onSubmitDeck: submitDeck,
      }}
      submissionError={submissionError}
      showSuggestions={false}
      responsiveLayout={responsiveLayout}
    />
  );
}

function CompleteView({ onLeave }: { onLeave: () => void }) {
  const { t } = useTranslation("draft");
  const navigate = useNavigate();
  // Three primitive selectors, matching this file's existing convention. The
  // component reads state and dispatches; it derives nothing — the seat count
  // the launch carries is read inside the store from `view.seats`.
  const view = useMultiplayerDraftStore((s) => s.view);
  const role = useMultiplayerDraftStore((s) => s.role);
  const launchCommanderGame = useMultiplayerDraftStore((s) => s.launchCommanderGame);
  // The engine procedure authorizes this launch; the page must not infer it
  // from a draft-kind label. Only the host holds the session the decks are
  // assembled from.
  const canLaunch = view?.launch_capability === "CommanderMultiplayer" && role === "host";
  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col items-center gap-6 py-8">
      {/* `launchCommanderGame` reports a payload refusal by writing `error` and
          NOT navigating, so without this banner that failure is invisible and
          the launch button reads as dead. Same placement as the other phase
          views (`PairingPhaseView`, `MatchInProgressView`, ...). */}
      <PodErrorBanner />
      <h1 className="menu-display text-3xl text-white">{t("podComplete.title")}</h1>
      <FormatStandings />
      {canLaunch && (
        <button
          onClick={() => void launchCommanderGame(navigate)}
          className={menuButtonClass({ tone: "indigo", size: "md" })}
        >
          {t("podComplete.launchCommanderGame")}
        </button>
      )}
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
  onRetry,
}: {
  phase: "error" | "kicked" | "hostLeft";
  onLeave: () => void;
  onRetry: () => void;
}) {
  const { t } = useTranslation("draft");
  const recoveryFailure = useMultiplayerDraftStore((s) => s.guestRecoveryFailure);
  const message =
    phase === "kicked"
      ? t("podError.kicked")
      : phase === "hostLeft"
        ? t("podError.hostLeft")
        : recoveryFailure?.message ?? t("podError.connection");
  return (
    <div className="flex flex-col items-center justify-center gap-4 py-24">
      <div className="text-xl font-medium text-red-300">{message}</div>
      {phase === "error" && recoveryFailure?.kind === "retryable" && (
        <button
          onClick={onRetry}
          className={menuButtonClass({ tone: "emerald", size: "md" })}
        >
          {t("podError.retry")}
        </button>
      )}
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
  screen: DraftPodScreen,
  onLeave: () => void,
  responsiveLayout: ResponsiveDraftLayout,
  phoneLayout: boolean,
  mobileWorkspaceOpen: boolean,
  setMobileWorkspaceOpen: (open: boolean) => void,
  onDismissOverlay: () => void,
  onRetry: () => void,
): React.ReactNode {
  // No `default` arm: `tsc` is what makes a future `DraftPodScreen` member
  // impossible to forget here.
  switch (screen) {
    case "idle":
    case "connecting":
      return <PodSetup />;
    case "lobby":
      return <DraftPodLobby onLeave={onLeave} />;
    case "drafting":
      return (
        <DraftingPhaseContent
          responsiveLayout={responsiveLayout}
          phoneLayout={phoneLayout}
          mobileWorkspaceOpen={mobileWorkspaceOpen}
          setMobileWorkspaceOpen={setMobileWorkspaceOpen}
        />
      );
    case "deckbuilding":
      return <PodDeckBuilder responsiveLayout={responsiveLayout} />;
    case "betweenGames":
      return <BetweenGamesView responsiveLayout={responsiveLayout} onDismiss={onDismissOverlay} />;
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
      return <PodErrorView phase={screen} onLeave={onLeave} onRetry={onRetry} />;
  }
}

// ── Page ───────────────────────────────────────────────────────────────

function DraftPodPageContent() {
  const { t } = useTranslation("draft");
  const phase = useMultiplayerDraftStore((s) => s.phase);
  const screen = useMultiplayerDraftStore(draftPodScreen);
  const promptKey = useMultiplayerDraftStore(intergamePromptKey);
  const playDrawPending = useMultiplayerDraftStore((s) => s.playDrawPrompt !== null);
  const [dismissedPromptKey, setDismissedPromptKey] = useState<string | null>(null);
  const sideboardPrompt = useMultiplayerDraftStore((s) => s.sideboardPrompt);
  const playDrawPrompt = useMultiplayerDraftStore((s) => s.playDrawPrompt);
  const sideboardSubmitted = useMultiplayerDraftStore((s) => s.sideboardSubmitted);
  const view = useMultiplayerDraftStore((s) => s.view);
  const intergameWorkspace = useMultiplayerDraftStore((s) => s.intergameWorkspaceState);
  const leave = useMultiplayerDraftStore((s) => s.leave);
  const resumeDraft = useMultiplayerDraftStore((s) => s.resumeDraft);
  const resetPod = useDraftPodStore((s) => s.reset);
  const resumeHostedPod = useDraftPodStore((s) => s.resumeHostedPod);
  const enterKindForEntry = useDraftPodStore((s) => s.enterKindForEntry);
  const navigate = useNavigate();
  const location = useLocation();
  const [searchParams] = useSearchParams();
  const entryGeneration = useRef(0);
  const retryController = useRef<AbortController | null>(null);
  const entry = searchParams.get("entry");
  const commanderDraftRequested = searchParams.get("kind") === COMMANDER_DRAFT_ENTRY;
  const resumeRequested = searchParams.get("resume") === "1";
  const entryMode = entry === "host" || entry === "guest" || entry === "auto"
    ? entry
    : resumeRequested ? "host" : "auto";
  const [responsiveViewport, setResponsiveViewport] = useState(() => ({
    width: window.innerWidth,
    height: window.innerHeight,
  }));
  const [podStatusOpen, setPodStatusOpen] = useState(false);
  const [mobileWorkspaceOpen, setMobileWorkspaceOpen] = useState(false);
  const endingDraftLatch = useRef(false);
  const [endingDraft, setEndingDraft] = useState(false);

  const responsiveLayout: ResponsiveDraftLayout = getResponsiveDraftLayout(
    responsiveViewport.width,
    responsiveViewport.height,
  );
  const phoneLayout = responsiveLayout === "phone-portrait" || responsiveLayout === "phone-landscape";
  const tabletLayout = responsiveLayout === "tablet-portrait" || responsiveLayout === "tablet-landscape";
  const compactHostControlsLayout = phoneLayout || tabletLayout;
  const phoneDrafting = phase === "drafting" && phoneLayout;
  const responsiveDrafting = phase === "drafting" && (phoneLayout || tabletLayout);
  const phoneDeckbuilding = phase === "deckbuilding" && phoneLayout;
  const handleEndDraft = useCallback(() => {
    if (endingDraftLatch.current) return;
    if (!window.confirm(t("hostControls.endDraftConfirm"))) return;

    endingDraftLatch.current = true;
    setEndingDraft(true);
    void (async () => {
      try {
        await leave(false);
        resetPod();
        navigate("/");
      } catch (err) {
        console.error("[DraftPodPage] failed to end draft:", err);
        endingDraftLatch.current = false;
        setEndingDraft(false);
      }
    })();
  }, [leave, navigate, resetPod, t]);
  const endDraftAction = useMemo<DraftShellTopAction>(() => ({
    id: "end-draft",
    label: t("hostControls.endDraft"),
    tone: "danger",
    disabled: endingDraft,
    onClick: handleEndDraft,
  }), [endingDraft, handleEndDraft, t]);
  const hostDraftTopActions = useHostDraftTopActions({
    enabled: phase === "drafting",
    endDraftAction,
  });
  const betweenGamesEditorActive = screen === "betweenGames"
    && sideboardPrompt !== null
    && view !== null
    && intergameWorkspace !== null
    && !sideboardSubmitted
    && playDrawPrompt === null;
  const tabletDeckbuilding = tabletLayout && (phase === "deckbuilding" || betweenGamesEditorActive);

  useEffect(() => {
    const refreshViewport = () => setResponsiveViewport({
      width: window.innerWidth,
      height: window.innerHeight,
    });
    window.addEventListener("resize", refreshViewport);
    window.addEventListener("orientationchange", refreshViewport);
    return () => {
      window.removeEventListener("resize", refreshViewport);
      window.removeEventListener("orientationchange", refreshViewport);
    };
  }, []);

  useEffect(() => {
    if (!responsiveDrafting) {
      setPodStatusOpen(false);
    }
  }, [responsiveDrafting]);

  useEffect(() => {
    if (!phoneLayout) {
      setMobileWorkspaceOpen(false);
    }
  }, [phoneLayout]);

  const handleOpenPodStatus = useCallback(() => {
    setPodStatusOpen(true);
  }, []);

  const phoneAction: DraftShellPhoneAction | undefined = useMemo(() => {
    if (!responsiveDrafting) return undefined;
    return {
      icon: <PodIcon className="h-6 w-6 opacity-70" />,
      label: t("landing.podInProgress"),
      onClick: handleOpenPodStatus,
    };
  }, [handleOpenPodStatus, responsiveDrafting, t]);

  useDraftShellChrome(
    phoneDrafting
      ? "phone-drafting"
      : phoneDeckbuilding
        ? "phone-deckbuilding"
        : tabletDeckbuilding
          ? "tablet-deckbuilding"
        : tabletLayout && phase === "drafting"
          ? "tablet-drafting"
          : "default",
    phoneAction,
    "pod",
    !(phase === "drafting" && responsiveLayout === "phone-portrait"),
    hostDraftTopActions,
  );

  useEffect(() => {
    const generation = entryGeneration;
    const routeToken = ++generation.current;
    const controller = new AbortController();

    void (async () => {
      if (entryMode === "host" || entryMode === "auto") {
        // A host locator gets first claim on automatic entry. A guest locator
        // is considered only after a terminal/invalid host locator has actually
        // been cleared, so a damaged host record cannot steal a guest's route.
        const outcome = await resumeHostedPod({
          silent: entryMode === "auto",
          routeToken,
          signal: controller.signal,
        });
        if (generation.current !== routeToken) return;
        if (entryMode === "host" || outcome === "resumed" || outcome === "superseded") return;

        if (outcome === "absent" || outcome === "terminal" || outcome === "invalid") {
          const guestOutcome: GuestDraftResumeOutcome = await resumeDraft({
            routeToken,
            signal: controller.signal,
          });
          if (generation.current !== routeToken || guestOutcome === "superseded") return;
          if (guestOutcome === "resumed" || guestOutcome === "failed") return;
        }
      } else {
        const guestOutcome: GuestDraftResumeOutcome = await resumeDraft({
          routeToken,
          signal: controller.signal,
        });
        if (generation.current !== routeToken || guestOutcome === "superseded") return;
        return;
      }
    })();
    return () => {
      controller.abort();
      retryController.current?.abort();
      retryController.current = null;
      if (generation.current === routeToken) generation.current++;
    };
  }, [entryMode, location.pathname, location.search, resumeDraft, resumeHostedPod]);

  const retryGuestRecovery = useCallback(() => {
    retryController.current?.abort();
    const controller = new AbortController();
    retryController.current = controller;
    const routeToken = ++entryGeneration.current;
    void resumeDraft({ routeToken, signal: controller.signal }).finally(() => {
      if (retryController.current === controller) retryController.current = null;
    });
  }, [resumeDraft]);

  useEffect(() => {
    // A resumed pod's kind comes from its persisted session, which is the higher
    // authority — a URL intent must never overwrite it.
    if (resumeRequested) return;
    if (!commanderDraftRequested) return;
    void enterKindForEntry("CommanderDraft");
  }, [commanderDraftRequested, enterKindForEntry, resumeRequested]);

  const handleLeave = useCallback(async () => {
    await leave(false);
    resetPod();
    navigate("/");
  }, [leave, resetPod, navigate]);

  // A dismissal is scoped to the prompt it was made about, so it expires by
  // construction when the next game's prompt — or the same game's play/draw
  // decision — arrives: no effect, no cleanup, and no latch that could outlive
  // the window it was hiding.
  const overlayDismissed = promptKey !== null && promptKey === dismissedPromptKey;
  const visibleScreen: DraftPodScreen = overlayDismissed ? phase : screen;

  // `showBack` stays on `phase`: it is `idle`/`connecting` only — both of which
  // `draftPodScreen` returns verbatim — and it is a session-level affordance,
  // not a screen-level one.
  const showBack = phase === "idle" || phase === "connecting";

  return (
    <div className={`menu-scene relative flex flex-col overflow-hidden ${phoneDrafting ? "h-dvh min-h-0 overscroll-none" : tabletLayout && phase === "drafting" ? "h-full min-h-0" : "min-h-screen"}`}>
      <ScreenChrome onBack={showBack ? handleLeave : undefined} />

      {/* Centered MenuShell column — same responsive framing as every other
          out-of-match surface. Each phase view renders its own heading, so no
          MenuShell title is passed. */}
      <MenuShell
        layout="stacked"
        contentWidthClass="max-w-none"
        compactTopPadding={
          (phoneLayout && (phase === "drafting" || phase === "deckbuilding"))
          || tabletDeckbuilding
        }
      >
        <div className="flex w-full flex-col">
          {screen === "betweenGames" && overlayDismissed && (
            <div
              role="status"
              className="mb-3 flex items-center justify-between gap-3 rounded-xl border border-amber-400/25 bg-amber-400/10 px-4 py-2"
            >
              <span className="text-sm text-white/70">
                {t(playDrawPending ? "betweenGames.hiddenNoticePlayDraw" : "betweenGames.hiddenNotice")}
              </span>
              <button
                onClick={() => setDismissedPromptKey(null)}
                className={menuButtonClass({ tone: "neutral", size: "xs" })}
              >
                {t("betweenGames.showOverlay")}
              </button>
            </div>
          )}
          {phaseContent(
            visibleScreen,
            handleLeave,
            responsiveLayout,
            phoneLayout,
            mobileWorkspaceOpen,
            setMobileWorkspaceOpen,
            () => setDismissedPromptKey(promptKey),
            retryGuestRecovery,
          )}
        </div>
      </MenuShell>

      {podStatusOpen && (
        <DialogShell
          title={t("landing.podInProgress")}
          onClose={() => setPodStatusOpen(false)}
          size="sm"
        >
          <SeatStatusRing />
        </DialogShell>
      )}

      {!(phase === "drafting" && compactHostControlsLayout) && (
        <HostControls
          draftTopActions={hostDraftTopActions}
          endDraftAction={endDraftAction}
        />
      )}
    </div>
  );
}

function DraftPodOfflineUnavailable() {
  const { t } = useTranslation(["draft", "menu"]);
  const navigate = useNavigate();

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <MenuShell
        title={t("offline.unavailableTitle", { ns: "draft" })}
        description={t("offline.unavailableDescription", { ns: "draft" })}
        layout="stacked"
      >
        <MenuPanel className="relative z-10 flex w-full max-w-3xl flex-col items-start gap-4 px-5 py-6">
          <button
            onClick={() => navigate("/")}
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
          >
            {t("nav.home", { ns: "menu" })}
          </button>
        </MenuPanel>
      </MenuShell>
    </div>
  );
}

export function DraftPodPage() {
  const effectiveOffline = useEffectiveOffline();
  const draftPodLive = useMultiplayerDraftStore(isMultiplayerDraftPodLive);

  if (effectiveOffline && !draftPodLive) return <DraftPodOfflineUnavailable />;

  return <DraftPodPageContent />;
}
