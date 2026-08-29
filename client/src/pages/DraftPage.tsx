import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import { useNavigate, useSearchParams } from "react-router";

import {
  useDraftStore,
  type DraftPackChoice,
  type DraftPickDestination,
  type DraftPickPlacementHint,
} from "../stores/draftStore";
import { setPackSequence } from "../adapter/draft-adapter";
import { usePreferencesStore } from "../stores/preferencesStore";
import type { CardHoverInfo } from "../components/card/CardPreview";
import { HoverCardPreview } from "../components/card/HoverCardPreview";
import { BotDifficultySelector } from "../components/draft/BotDifficultySelector";
import { CubeSetupPanel } from "../components/draft/CubeSetupPanel";
import { DraftIntro } from "../components/draft/DraftIntro";
import { DraftSteps } from "../components/draft/DraftSteps";
import { SetSelector } from "../components/draft/SetSelector";
import { PackDisplay, type PackDisplayController } from "../components/draft/PackDisplay";
import { DraftWorkspace } from "../components/draft/workspace/DraftWorkspace";
import { resolveWorkspacePickPlacement } from "../components/draft/workspace/workspacePlacement";
import {
  useDraftWorkspaceDrag,
  type DraftDropRequest,
  type DraftPickInteractionSnapshot,
} from "../components/draft/workspace/useDraftWorkspaceDrag";
import {
  getResponsiveDraftLayout,
  loadDraftWorkspacePreferences,
  repairDraftWorkspacePackScale,
  saveDraftWorkspacePreferences,
  type DraftWorkspacePreferences,
  type ResponsiveDraftLayout,
} from "../components/draft/workspace/workspacePreferences";
import { DraftProgress } from "../components/draft/DraftProgress";
import { LimitedDeckBuilder } from "../components/draft/LimitedDeckBuilder";
import { SealedPackOpening } from "../components/draft/SealedPackOpening";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { useDraftShellChrome } from "../components/chrome/ShellContext";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { MenuShell } from "../components/menu/MenuShell";
import { runLimits } from "../services/quickDraftPersistence";
import type { DraftRunFormat, DraftRunState } from "../services/quickDraftPersistence";

// ── Format Picker ─────────────────────────────────────────────────────

const FORMAT_OPTIONS: Array<{ value: DraftRunFormat; labelKey: string; descKey: string }> = [
  { value: "single", labelKey: "formatPicker.single.label", descKey: "formatPicker.single.description" },
  { value: "bo3", labelKey: "formatPicker.bo3.label", descKey: "formatPicker.bo3.description" },
  { value: "run", labelKey: "formatPicker.run.label", descKey: "formatPicker.run.description" },
];

type DraftSetupMode = "quick" | "sealed" | "cube";

/** Boosters a Sealed event opens — fixed by the engine (`SEALED_PACK_COUNT`). */
const SEALED_PACK_COUNT = 6;
/** Boosters a draft opens by default; the player may line up a different count. */
const DRAFT_PACK_COUNT = 3;
function readPickInteraction(): DraftPickInteractionSnapshot {
  const state = useDraftStore.getState();
  return {
    interactionGeneration: state.interactionGeneration,
    pickInteractionLocked: state.pickInteractionLocked,
    pendingPickIntent: state.pendingPickIntent,
  };
}

function subscribePickInteraction(listener: () => void): () => void {
  return useDraftStore.subscribe((state, previous) => {
    if (
      state.interactionGeneration !== previous.interactionGeneration
      || state.pickInteractionLocked !== previous.pickInteractionLocked
      || state.pendingPickIntent !== previous.pendingPickIntent
    ) listener();
  });
}

function FormatPicker({ onLaunch, supportsBo3 }: { onLaunch: () => void; supportsBo3: boolean }) {
  const { t } = useTranslation("draft");
  const runFormat = useDraftStore((s) => s.runFormat);
  const setRunFormat = useDraftStore((s) => s.setRunFormat);

  return (
    <div className="flex flex-col items-center gap-8 py-16">
      <div className="text-center">
        <h1 className="menu-display text-3xl text-white">{t("formatPicker.title")}</h1>
        <p className="mt-2 text-sm text-white/45">{t("formatPicker.subtitle")}</p>
      </div>

      <div className="flex w-full max-w-lg flex-col gap-3">
        {(supportsBo3 ? FORMAT_OPTIONS : FORMAT_OPTIONS.filter((opt) => opt.value !== "bo3")).map((opt) => (
          <button
            key={opt.value}
            type="button"
            onClick={() => setRunFormat(opt.value)}
            className={`group flex w-full cursor-pointer items-start gap-4 rounded-card border surface-card p-4 text-left transition-all duration-150 ${
              runFormat === opt.value
                ? "border-jade/45 ring-1 ring-jade/20 shadow-panel"
                : "border-hairline hover:-translate-y-[3px] hover:border-hairline-hover hover:shadow-panel"
            }`}
          >
            <div
              className={`mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded-full border-2 transition-colors ${
                runFormat === opt.value
                  ? "border-jade bg-jade"
                  : "border-fg-muted/50"
              }`}
            >
              {runFormat === opt.value && (
                <div className="h-2 w-2 rounded-full bg-gray-950" />
              )}
            </div>
            <div className="min-w-0 flex-1">
              <div className={`font-display text-base font-semibold ${runFormat === opt.value ? "text-jade-text" : "text-fg"}`}>
                {t(opt.labelKey)}
              </div>
              <p className="mt-1 text-sm text-fg-card-body">{t(opt.descKey)}</p>
            </div>
          </button>
        ))}
      </div>

      <button
        type="button"
        onClick={onLaunch}
        className={menuButtonClass({ tone: "emerald", size: "lg" })}
      >
        {t("formatPicker.startMatch")}
      </button>
    </div>
  );
}

// ── Between Matches ───────────────────────────────────────────────────

function BetweenMatches({ onNext, onEnd }: { onNext: () => void; onEnd: () => void }) {
  const { t } = useTranslation("draft");
  const runState = useDraftStore((s) => s.runState);
  const runFormat = useDraftStore((s) => s.runFormat);

  if (!runState) return null;

  const { wins, losses, draws } = tallyResults(runState.results);
  const limits = runLimits(runFormat);
  const matchNumber = runState.results.length + 1;

  return (
    <div className="flex flex-col items-center gap-8 py-16">
      <h1 className="menu-display text-3xl text-white">{t("run.draftRun")}</h1>

      <RecordSummary wins={wins} losses={losses} draws={draws} limits={limits} />

      <MatchHistory results={runState.results} />

      <p className="text-sm text-white/45">{t("run.upNext", { number: matchNumber })}</p>

      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={onNext}
          className={menuButtonClass({ tone: "emerald", size: "lg" })}
        >
          {t("run.nextMatch")}
        </button>
        <button
          type="button"
          onClick={onEnd}
          className={menuButtonClass({ tone: "neutral", size: "md" })}
        >
          {t("run.endRun")}
        </button>
      </div>
    </div>
  );
}

// ── Run Complete ──────────────────────────────────────────────────────

function RunComplete({ onDone }: { onDone: () => void }) {
  const { t } = useTranslation("draft");
  const runState = useDraftStore((s) => s.runState);
  const runFormat = useDraftStore((s) => s.runFormat);

  if (!runState) return null;

  const { wins, losses, draws } = tallyResults(runState.results);
  const limits = runLimits(runFormat);
  const hitMaxWins = wins >= limits.maxWins;
  const perfect = hitMaxWins && losses === 0;

  return (
    <motion.div
      initial={{ opacity: 0, y: 12 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.4, ease: "easeOut" }}
      className="flex flex-col items-center gap-8 py-16"
    >
      <div className="relative flex flex-col items-center gap-2">
        {hitMaxWins && (
          <motion.div
            aria-hidden="true"
            className="pointer-events-none absolute -inset-x-20 -inset-y-10 rounded-full bg-emerald-400/15 blur-3xl"
            initial={{ opacity: 0, scale: 0.6 }}
            animate={{ opacity: 1, scale: 1 }}
            transition={{ delay: 0.15, duration: 0.6, ease: "easeOut" }}
          />
        )}
        <h1 className="menu-display relative text-3xl text-white">
          {perfect ? t("run.perfectRun") : hitMaxWins ? t("run.runComplete") : t("run.runOver")}
        </h1>
        <p className="relative text-white/55">
          {hitMaxWins
            ? perfect
              ? t("run.finishedFlawless", { wins, losses })
              : t("run.finishedCongrats", { wins, losses })
            : t("run.finishedRecord", { wins, losses })}
        </p>
      </div>

      <RecordSummary wins={wins} losses={losses} draws={draws} limits={limits} />

      <MatchHistory results={runState.results} />

      <button
        type="button"
        onClick={onDone}
        className={menuButtonClass({ tone: "neutral", size: "lg" })}
      >
        {t("run.done")}
      </button>
    </motion.div>
  );
}

// ── Shared sub-components ─────────────────────────────────────────────

function tallyResults(results: DraftRunState["results"]): { wins: number; losses: number; draws: number } {
  let wins = 0;
  let losses = 0;
  let draws = 0;
  for (const r of results) {
    if (r.result === "win") wins += 1;
    else if (r.result === "loss") losses += 1;
    else draws += 1;
  }
  return { wins, losses, draws };
}

function RecordSummary({
  wins,
  losses,
  draws,
  limits,
}: {
  wins: number;
  losses: number;
  draws: number;
  limits: { maxWins: number; maxLosses: number };
}) {
  const { t } = useTranslation("draft");
  return (
    <div className="flex flex-col items-center gap-2">
      <div className="flex items-center gap-8">
        <RecordTrack label={t("run.wins")} count={wins} max={limits.maxWins} color="emerald" />
        <RecordTrack label={t("run.losses")} count={losses} max={limits.maxLosses} color="red" />
      </div>
      {draws > 0 && (
        <span className="text-xs uppercase tracking-wider text-white/35">
          {t("run.drawCount", { count: draws })}
        </span>
      )}
    </div>
  );
}

function RecordTrack({
  label,
  count,
  max,
  color,
}: {
  label: string;
  count: number;
  max: number;
  color: "emerald" | "red";
}) {
  const palette = {
    emerald: { filled: "border-emerald-300 bg-emerald-400 shadow-[0_0_8px] shadow-emerald-400/50", empty: "border-emerald-400/25", text: "text-emerald-200" },
    red: { filled: "border-red-300 bg-red-400 shadow-[0_0_8px] shadow-red-400/50", empty: "border-red-400/25", text: "text-red-200" },
  }[color];
  return (
    <div className="flex flex-col items-center gap-2">
      <div className="flex items-center gap-1.5">
        {Array.from({ length: max }, (_, i) => (
          <span
            key={i}
            className={`h-3.5 w-3.5 rounded-full border transition-colors duration-300 ${i < count ? palette.filled : palette.empty}`}
          />
        ))}
      </div>
      <span className={`text-xs uppercase tracking-wider opacity-70 ${palette.text}`}>
        {label} {count}/{max}
      </span>
    </div>
  );
}

function MatchHistory({ results }: { results: DraftRunState["results"] }) {
  const { t } = useTranslation("draft");
  if (results.length === 0) return null;
  return (
    <div className="flex flex-col items-center gap-2">
      <span className="text-[0.62rem] font-medium uppercase tracking-[0.18em] text-white/30">{t("run.matchLog")}</span>
      <div className="flex items-center gap-1">
        {results.map((r, i) => (
          <div
            key={r.gameId}
            className={`flex h-7 w-7 items-center justify-center rounded-md text-[11px] font-bold ${
              r.result === "win"
                ? "bg-emerald-500/18 text-emerald-300"
                : r.result === "loss"
                  ? "bg-red-500/18 text-red-300"
                  : "bg-slate-500/18 text-slate-300"
            }`}
            title={t("run.matchResultTitle", {
              number: i + 1,
              result: t(`run.result.${r.result}`),
            })}
          >
            {r.result === "win"
              ? t("run.resultShort.win")
              : r.result === "loss"
                ? t("run.resultShort.loss")
                : t("run.resultShort.draw")}
          </div>
        ))}
      </div>
    </div>
  );
}

// ── Main Component ────────────────────────────────────────────────────

export function DraftPage() {
  const { t } = useTranslation("draft");
  const phase = useDraftStore((s) => s.phase);
  const draftView = useDraftStore((s) => s.view);
  const selectedCard = useDraftStore((s) => s.selectedCard);
  const workspaceState = useDraftStore((s) => s.workspaceState);
  const pendingPickIntent = useDraftStore((s) => s.pendingPickIntent);
  const interactionGeneration = useDraftStore((s) => s.interactionGeneration);
  const pickInteractionLocked = useDraftStore((s) => s.pickInteractionLocked);
  const draftCardPreviewMode = usePreferencesStore((s) => s.draftCardPreviewMode);
  const draftDoubleClickConfirmPick = usePreferencesStore((s) => s.draftDoubleClickConfirmPick);
  const reset = useDraftStore((s) => s.reset);
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const requestedSetupMode = searchParams.get("mode");
  const [hoveredCard, setHoveredCard] = useState<CardHoverInfo | null>(null);
  const [introDismissed, setIntroDismissed] = useState(false);
  const [resumeLoading, setResumeLoading] = useState(false);
  const [workspacePreferences, setWorkspacePreferences] = useState<DraftWorkspacePreferences>(loadDraftWorkspacePreferences);
  const [responsiveViewport, setResponsiveViewport] = useState(() => ({
    width: window.innerWidth,
    height: window.innerHeight,
  }));
  const [mobileWorkspaceOpen, setMobileWorkspaceOpen] = useState(false);
  const [setupMode, setSetupMode] = useState<DraftSetupMode>(() =>
    requestedSetupMode === "cube" || requestedSetupMode === "sealed"
      ? requestedSetupMode
      : "quick",
  );
  const responsiveLayout: ResponsiveDraftLayout = getResponsiveDraftLayout(
    responsiveViewport.width,
    responsiveViewport.height,
  );
  const phoneLayout = responsiveLayout === "phone-portrait" || responsiveLayout === "phone-landscape";
  const tabletLayout = responsiveLayout === "tablet-portrait" || responsiveLayout === "tablet-landscape";
  const responsiveDrafting = phase === "drafting" && responsiveLayout !== "desktop";
  const phoneDeckbuilding = phase === "deckbuilding" && phoneLayout;
  const tabletDeckbuilding = phase === "deckbuilding" && tabletLayout;
  const compactSteps = responsiveDrafting || phoneDeckbuilding || tabletDeckbuilding;
  useDraftShellChrome(
    phoneLayout && phase === "drafting"
      ? "phone-drafting"
      : phoneDeckbuilding
        ? "phone-deckbuilding"
        : tabletDeckbuilding
          ? "tablet-deckbuilding"
        : responsiveDrafting
          ? "tablet-drafting"
        : "default",
  );

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
    if (responsiveLayout !== "phone-portrait" && responsiveLayout !== "phone-landscape") {
      setMobileWorkspaceOpen(false);
    }
  }, [responsiveLayout]);

  useEffect(() => {
    if (searchParams.get("resume") !== "1") return;
    let cancelled = false;

    async function doResume() {
      setResumeLoading(true);
      try {
        await useDraftStore.getState().resumeDraft();
        if (!cancelled) setIntroDismissed(true);
      } catch {
        await useDraftStore.getState().abandonDraft();
      } finally {
        if (!cancelled) setResumeLoading(false);
      }
    }
    doResume();
    return () => { cancelled = true; };
  }, [searchParams]);

  useEffect(() => {
    setSetupMode(
      requestedSetupMode === "cube" || requestedSetupMode === "sealed"
        ? requestedSetupMode
        : "quick",
    );
  }, [requestedSetupMode]);

  useEffect(() => {
    return () => {
      reset();
    };
  }, [reset]);

  const handleStartDraft = useCallback(
    async (packs: DraftPackChoice[]) => {
      const { difficulty, startDraft, startSealedDraft } = useDraftStore.getState();

      const resp = await fetch(__DRAFT_POOLS_URL__);
      if (!resp.ok) throw new Error(`Failed to load draft pools: ${resp.status}`);
      const allPools: Record<string, unknown> = await resp.json();

      // One pool per distinct set — a set drafted in several packs still
      // crosses the WASM boundary once. `sequence` (built in the store from
      // `packs`) is what repeats.
      const { pools } = setPackSequence(packs, allPools);

      const selection = { packs, pools };
      if (setupMode === "sealed") {
        await startSealedDraft(selection, difficulty);
      } else {
        await startDraft(selection, difficulty);
      }
    },
    [setupMode],
  );

  const handleLaunchMatch = useCallback(async () => {
    await useDraftStore.getState().launchMatch(navigate);
  }, [navigate]);

  const handleLaunchNextMatch = useCallback(async () => {
    await useDraftStore.getState().launchNextMatch(navigate);
  }, [navigate]);

  const handleEndRun = useCallback(async () => {
    await useDraftStore.getState().endRun();
    navigate("/draft");
  }, [navigate]);

  const handleWorkspacePreferencesChange = useCallback((next: DraftWorkspacePreferences) => {
    if (useDraftStore.getState().pickInteractionLocked) return;
    setWorkspacePreferences(next);
    saveDraftWorkspacePreferences(next);
  }, []);

  const handleDrop = useCallback((request: DraftDropRequest) => {
    const state = useDraftStore.getState();
    const outcome = request.source.kind === "pick"
      ? state.pickCard(request.source.instanceIds[0], request.destination, request.placementHint)
      : state.pickCardWithDraftEffect(
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
    const placement = useDraftStore.getState().workspaceState?.placements[sourceInstanceId];
    return Math.min(
      workspacePreferences.sideboard.columnCount - 1,
      Math.max(0, placement?.column ?? 0),
    );
  }, [workspacePreferences.sideboard.columnCount]);

  const dragController = useDraftWorkspaceDrag({
    enabled: phase === "drafting" && introDismissed && !pickInteractionLocked,
    readPickInteraction,
    subscribePickInteraction,
    onDrop: handleDrop,
    resolveCollapsedSideboardColumn,
  });

  const handleConfirmPick = useCallback((
    destination: DraftPickDestination,
    placementHint?: DraftPickPlacementHint,
  ) => {
    const state = useDraftStore.getState();
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
    const state = useDraftStore.getState();
    const { view, workspaceState } = state;
    const placementHints = view !== null && view.current_pack !== null && workspaceState !== null
      ? Object.fromEntries(view.current_pack.map((card) => [
        card.instance_id,
        resolveWorkspacePickPlacement(
          card,
          "deck",
          view.pool,
          view.pool_groups,
          workspaceState,
          workspacePreferences.deck,
        ),
      ]))
      : undefined;
    return state.autoPickCard("deck", placementHints);
  }, [workspacePreferences.deck]);

  const packController = useMemo<PackDisplayController>(() => ({
    kind: "local-workspace",
    view: draftView,
    selectedCard,
    pendingIntent: pendingPickIntent,
    interactionGeneration,
    interactionLocked: pickInteractionLocked,
    doubleClickPick: draftDoubleClickConfirmPick,
    dragController,
    selectCard: (instanceId) => useDraftStore.getState().selectCard(instanceId),
    pickCard: (instanceId, destination, placementHint) => useDraftStore.getState().pickCard(instanceId, destination, placementHint),
    pickCardStep: (instanceIds, destination, placementHint) => instanceIds.length === 1
      ? useDraftStore.getState().pickCard(instanceIds[0], destination, placementHint)
      : Promise.resolve({ status: "rejected", reason: "invalid-request" }),
    confirmPick: handleConfirmPick,
    pickCardWithDraftEffect: (effectInstanceId, instanceIds, destination, placementHint) => useDraftStore.getState().pickCardWithDraftEffect(effectInstanceId, instanceIds, destination, placementHint),
    autoPickCard: handleAutoPick,
  }), [
    draftView, dragController, handleAutoPick, handleConfirmPick, interactionGeneration, pendingPickIntent, pickInteractionLocked,
    selectedCard, draftDoubleClickConfirmPick,
  ]);

  const packPresentation = useMemo(() => ({
    packScale: workspacePreferences.packScale,
    setPackScale: (next: number) => handleWorkspacePreferencesChange({
      ...workspacePreferences,
      packScale: repairDraftWorkspacePackScale(next),
    }),
  }), [handleWorkspacePreferencesChange, workspacePreferences]);

  return (
    <div className={`menu-scene relative flex flex-col overflow-hidden ${phoneLayout && phase === "drafting" && introDismissed ? "h-dvh min-h-0 overscroll-none" : tabletLayout && phase === "drafting" && introDismissed ? "h-full min-h-0" : "min-h-screen"}`}>
      <ScreenChrome onBack={() => navigate("/draft")} />
      {phase === "drafting" && introDismissed && (
        <HoverCardPreview
          card={hoveredCard}
          mode={draftCardPreviewMode}
          hoverDelayMs={0}
        />
      )}

        {/* Keep the shell's responsive padding while allowing card-heavy draft
          phases to use all available width. Narrow setup phases retain their
          own local max-widths. */}
        <MenuShell
          layout="stacked"
          contentWidthClass="max-w-none"
          compactTopPadding={
            (phoneLayout && (phase === "drafting" || phase === "deckbuilding"))
            || tabletDeckbuilding
          }
        >
        <div className="flex w-full flex-col">
        {resumeLoading ? (
          <div className="flex items-center justify-center py-24">
            <div className="h-8 w-8 animate-spin rounded-full border-2 border-gray-500 border-t-white" />
          </div>
        ) : !compactSteps ? (
          <div
            className={phase === "drafting" && introDismissed
                ? "mb-4"
                : "mb-12"}
            data-draft-steps-spacing
          >
            <DraftSteps phase={phase} />
          </div>
        ) : null}

        {!resumeLoading && phase === "setup" && (
          <div className="mx-auto w-full max-w-4xl">
            <h1 className="mb-8 menu-display text-3xl text-white">
              {setupMode === "cube"
                ? t("page.cubeDraftTitle")
                : setupMode === "sealed"
                  ? t("page.sealedTitle")
                  : t("page.quickDraftTitle")}
            </h1>
            <div className="mb-5 inline-flex rounded-lg border border-white/10 bg-black/25 p-1">
              {(["quick", "sealed", "cube"] as const).map((mode) => (
                <button
                  key={mode}
                  type="button"
                  onClick={() => setSetupMode(mode)}
                  className={`min-h-11 rounded-md px-4 py-2 text-sm font-medium transition-colors ${
                    setupMode === mode
                      ? "bg-emerald-400/18 text-emerald-100"
                      : "text-white/50 hover:bg-white/6 hover:text-white/75"
                  }`}
                >
                  {mode === "quick"
                    ? t("page.quickDraftTitle")
                    : mode === "sealed"
                      ? t("page.sealedTitle")
                      : t("page.cubeDraftTitle")}
                </button>
              ))}
            </div>
            {setupMode === "cube" ? (
              <div className="flex flex-col gap-6">
                <BotDifficultySelector />
                <CubeSetupPanel
                  onStart={async ({ cubeName, cubeListText, settings }) => {
                    const { difficulty, startCubeDraft } = useDraftStore.getState();
                    await startCubeDraft(cubeListText, cubeName, settings, difficulty);
                  }}
                />
              </div>
            ) : (
              <SetSelector
                onStartDraft={handleStartDraft}
                defaultPackCount={setupMode === "sealed" ? SEALED_PACK_COUNT : DRAFT_PACK_COUNT}
                fixedPackCount={setupMode === "sealed"}
                startLabel={
                  setupMode === "sealed"
                    ? t("setSelector.startSealed")
                    : t("setSelector.startDraft")
                }
              />
            )}
          </div>
        )}

        {phase === "drafting" && !introDismissed && (
          <DraftIntro
            mode="quick"
            packCount={draftView?.pack_count}
            cardsPerPack={draftView?.cards_per_pack}
            packSizes={draftView?.pack_sizes}
            onContinue={() => setIntroDismissed(true)}
          />
        )}

        {phase === "drafting" && introDismissed && (
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
            <div className={responsiveLayout === "desktop" ? "w-full min-w-0" : "h-full min-h-0 w-full min-w-0 overflow-hidden"}>
              {responsiveLayout === "desktop" && (
                <DraftProgress />
              )}
              <PackDisplay
                controller={packController}
                presentation={packPresentation}
                onCardHover={setHoveredCard}
                responsiveLayout={responsiveLayout}
                phoneToolbarPinned={phoneLayout && !mobileWorkspaceOpen}
                mobileWorkspaceOpen={mobileWorkspaceOpen}
                enableDraftEffects
              />
            </div>
            {draftView && workspaceState && (
              <div className={responsiveLayout === "desktop"
                ? "w-full min-w-0"
                : phoneLayout
                  ? "h-0 min-h-0 w-full min-w-0"
                  : "h-full min-h-0 w-full min-w-0"}
              >
                <DraftWorkspace
                  pool={draftView.pool}
                  poolGroups={draftView.pool_groups}
                  workspace={workspaceState}
                  preferences={workspacePreferences}
                  interactionLocked={pickInteractionLocked}
                  dragController={dragController}
                  responsiveLayout={responsiveLayout}
                  mobileOverlay
                  mobileWorkspaceOpen={mobileWorkspaceOpen}
                  onMobileWorkspaceOpenChange={setMobileWorkspaceOpen}
                  onWorkspaceChange={(next) => useDraftStore.getState().setWorkspaceState(next)}
                  onPreferencesChange={handleWorkspacePreferencesChange}
                  onCardHover={setHoveredCard}
                />
              </div>
            )}
          </div>
        )}

        {phase === "deckbuilding" && draftView && workspaceState && (
          <LimitedDeckBuilder
            responsiveLayout={responsiveLayout}
            local={{
              view: draftView,
              workspace: workspaceState,
              preferences: workspacePreferences,
              interactionLocked: pickInteractionLocked,
              onWorkspaceChange: (next) => useDraftStore.getState().setWorkspaceState(next),
              onPreferencesChange: handleWorkspacePreferencesChange,
              onAddBasicLand: (name) => useDraftStore.getState().addBasicLand(name),
              onRemoveBasicLand: (name) => useDraftStore.getState().removeBasicLand(name),
              onAutoSuggestDeck: () => useDraftStore.getState().autoSuggestDeck(),
              onAutoSuggestLands: () => useDraftStore.getState().autoSuggestLands(),
              onSubmitDeck: () => useDraftStore.getState().submitDeck(),
              onCardHover: setHoveredCard,
            }}
          />
        )}

        {phase === "opening" && draftView && (
          <SealedPackOpening
            view={draftView}
            onComplete={() => useDraftStore.getState().completeSealedOpening()}
          />
        )}

        {phase === "launching" && (
          <FormatPicker
            onLaunch={handleLaunchMatch}
            supportsBo3={draftView?.match_config.match_type === "Bo3"}
          />
        )}

        {!resumeLoading && phase === "playing" && (
          <BetweenMatches onNext={handleLaunchNextMatch} onEnd={handleEndRun} />
        )}

        {!resumeLoading && phase === "complete" && (
          <RunComplete onDone={handleEndRun} />
        )}
        </div>
      </MenuShell>
    </div>
  );
}
