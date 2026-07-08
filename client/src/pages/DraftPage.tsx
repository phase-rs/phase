import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import { useNavigate, useSearchParams } from "react-router";

import { useDraftStore } from "../stores/draftStore";
import { CardPreview } from "../components/card/CardPreview";
import type { CardHoverInfo } from "../components/card/CardPreview";
import { BotDifficultySelector } from "../components/draft/BotDifficultySelector";
import { CubeSetupPanel } from "../components/draft/CubeSetupPanel";
import { DraftIntro } from "../components/draft/DraftIntro";
import { DraftSteps } from "../components/draft/DraftSteps";
import { SetSelector } from "../components/draft/SetSelector";
import { PackDisplay } from "../components/draft/PackDisplay";
import { PoolPanel } from "../components/draft/PoolPanel";
import { DraftProgress } from "../components/draft/DraftProgress";
import { LimitedDeckBuilder } from "../components/draft/LimitedDeckBuilder";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
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

type DraftSetupMode = "set" | "cube";

function FormatPicker({ onLaunch }: { onLaunch: () => void }) {
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
        {FORMAT_OPTIONS.map((opt) => (
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
  const reset = useDraftStore((s) => s.reset);
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const requestedSetupMode = searchParams.get("mode");
  const [hoveredCard, setHoveredCard] = useState<CardHoverInfo | null>(null);
  const [introDismissed, setIntroDismissed] = useState(false);
  const [resumeLoading, setResumeLoading] = useState(false);
  const [setupMode, setSetupMode] = useState<DraftSetupMode>(() =>
    requestedSetupMode === "cube" ? "cube" : "set",
  );

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
    setSetupMode(requestedSetupMode === "cube" ? "cube" : "set");
  }, [requestedSetupMode]);

  useEffect(() => {
    return () => {
      reset();
    };
  }, [reset]);

  const handleStartDraft = useCallback(
    async (setCode: string, setName: string) => {
      const { difficulty, startDraft } = useDraftStore.getState();

      const resp = await fetch(__DRAFT_POOLS_URL__);
      if (!resp.ok) throw new Error(`Failed to load draft pools: ${resp.status}`);
      const allPools: Record<string, unknown> = await resp.json();
      const setPool = allPools[setCode.toLowerCase()] ?? allPools[setCode.toUpperCase()];
      if (!setPool) throw new Error(`No pool data for set: ${setCode}`);

      await startDraft(JSON.stringify(setPool), setCode, setName, difficulty);
    },
    [],
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

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <ScreenChrome onBack={() => navigate("/draft")} />
      {phase === "drafting" && introDismissed && (
        <CardPreview cardName={hoveredCard?.name ?? null} sourcePrinting={hoveredCard?.sourcePrinting} />
      )}

      {/* Centered MenuShell column — identical framing to home/setup/online so
          the draft flow sits in the same responsive, centered container. The
          per-phase blocks below render their own headings, so no MenuShell
          title is passed. */}
      <MenuShell layout="stacked">
        <div className="flex w-full flex-col">
        {resumeLoading ? (
          <div className="flex items-center justify-center py-24">
            <div className="h-8 w-8 animate-spin rounded-full border-2 border-gray-500 border-t-white" />
          </div>
        ) : (
          <div className="mb-12">
            <DraftSteps phase={phase} />
          </div>
        )}

        {!resumeLoading && phase === "setup" && (
          <div className="mx-auto w-full max-w-4xl">
            <h1 className="mb-8 menu-display text-3xl text-white">
              {setupMode === "cube" ? t("page.cubeDraftTitle") : t("page.quickDraftTitle")}
            </h1>
            <div className="mb-5 inline-flex rounded-lg border border-white/10 bg-black/25 p-1">
              {(["set", "cube"] as const).map((mode) => (
                <button
                  key={mode}
                  type="button"
                  onClick={() => setSetupMode(mode)}
                  className={`rounded-md px-4 py-2 text-sm font-medium transition-colors ${
                    setupMode === mode
                      ? "bg-emerald-400/18 text-emerald-100"
                      : "text-white/50 hover:bg-white/6 hover:text-white/75"
                  }`}
                >
                  {mode === "set" ? t("page.setDraftTab") : t("page.cubeTab")}
                </button>
              ))}
            </div>
            {setupMode === "set" ? (
              <SetSelector onStartDraft={handleStartDraft} />
            ) : (
              <div className="flex flex-col gap-6">
                <BotDifficultySelector />
                <CubeSetupPanel
                  onStart={async ({ cubeName, cubeListText, settings }) => {
                    const { difficulty, startCubeDraft } = useDraftStore.getState();
                    await startCubeDraft(cubeListText, cubeName, settings, difficulty);
                  }}
                />
              </div>
            )}
          </div>
        )}

        {phase === "drafting" && !introDismissed && (
          <DraftIntro mode="quick" onContinue={() => setIntroDismissed(true)} />
        )}

        {phase === "drafting" && introDismissed && (
          <div className="flex gap-4">
            <div className="flex-1">
              <div className="mb-4">
                <DraftProgress />
              </div>
              <PackDisplay onCardHover={setHoveredCard} showAutoPick />
            </div>
            <PoolPanel onCardHover={setHoveredCard} />
          </div>
        )}

        {phase === "deckbuilding" && (
          <LimitedDeckBuilder />
        )}

        {phase === "launching" && (
          <FormatPicker onLaunch={handleLaunchMatch} />
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
