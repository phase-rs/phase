import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type { GameFormat, MatchType } from "../../adapter/types";
import { AI_DIFFICULTIES, type AIDifficulty } from "../../constants/ai";
import type { AiDeckCandidate } from "../../services/aiDeckCatalog";
import { filterByBracket, useAiDeckCatalog } from "../../services/aiDeckCatalog";
import { CEDH_BRACKET } from "../../services/cedhLock";
import { isCommanderFamilyFormat } from "../../types/bracket";
import {
  AI_DECK_RANDOM,
  usePreferencesStore,
  type AiArchetypeFilter,
  type AiDeckSelection,
} from "../../stores/preferencesStore";
import { SelectField } from "../ui/SelectField";
import type { DeckArchetype } from "../../services/engineRuntime";
import { BracketFilter } from "./BracketFilter";

interface Props {
  selectedFormat?: GameFormat | null;
  selectedMatchType?: MatchType;
  /** Number of AI opponents to configure (i.e. playerCount - 1). Defaults to 1
   *  so the component still renders sensibly when mounted outside the setup
   *  page's player-count context. */
  opponentCount?: number;
  onCandidateCountChange?: (count: number | null) => void;
}

const ARCHETYPE_OPTIONS: AiArchetypeFilter[] = [
  "Any",
  "Aggro",
  "Midrange",
  "Control",
  "Combo",
  "Ramp",
];

function archetypeAccent(a: DeckArchetype | null): string {
  switch (a) {
    case "Aggro":
      return "text-red-300";
    case "Control":
      return "text-sky-300";
    case "Midrange":
      return "text-emerald-300";
    case "Combo":
      return "text-fuchsia-300";
    case "Ramp":
      return "text-amber-300";
    default:
      return "text-slate-400";
  }
}

export function AiOpponentConfig({
  selectedFormat,
  selectedMatchType,
  opponentCount = 1,
  onCandidateCountChange,
}: Props) {
  const { t } = useTranslation("menu");
  const aiSeats = usePreferencesStore((s) => s.aiSeats);
  const cedhMode = usePreferencesStore((s) => s.cedhMode);
  const setCedhMode = usePreferencesStore((s) => s.setCedhMode);
  const setAiSeatDifficulty = usePreferencesStore((s) => s.setAiSeatDifficulty);
  const setAiSeatDeckId = usePreferencesStore((s) => s.setAiSeatDeckId);
  const ensureAiSeatCount = usePreferencesStore((s) => s.ensureAiSeatCount);
  const archetypeFilter = usePreferencesStore((s) => s.aiArchetypeFilter);
  const setArchetypeFilter = usePreferencesStore((s) => s.setAiArchetypeFilter);
  const coverageFloor = usePreferencesStore((s) => s.aiCoverageFloor);
  const setCoverageFloor = usePreferencesStore((s) => s.setAiCoverageFloor);
  const bracketFilter = usePreferencesStore((s) => s.aiBracketFilter);
  const setBracketFilter = usePreferencesStore((s) => s.setAiBracketFilter);
  const isCedhFormat = isCommanderFamilyFormat(selectedFormat);
  const effectiveCedhMode = cedhMode && isCedhFormat;

  // Keep the persisted seat list in sync with the setup page's player count.
  useEffect(() => {
    ensureAiSeatCount(opponentCount);
  }, [opponentCount, ensureAiSeatCount]);

  // cEDH mode is a persisted preference read directly at game start
  // (GameProvider → effectiveAiDifficulty). When the format is known to be a
  // non-Commander variant the toggle is hidden, so clear any stale enabled
  // state to stop it silently forcing cEDH difficulty on non-Commander tables.
  // Guard on a truthy format so the brief format-loading window doesn't clobber
  // a legitimate flag.
  useEffect(() => {
    if (selectedFormat && !isCedhFormat && cedhMode) {
      setCedhMode(false);
    }
  }, [selectedFormat, isCedhFormat, cedhMode, setCedhMode]);

  const { candidates, loading, error } = useAiDeckCatalog({ selectedFormat, selectedMatchType });

  useEffect(() => {
    onCandidateCountChange?.(loading ? null : candidates.length);
  }, [candidates.length, loading, onCandidateCountChange]);

  // The archetype + coverage filters only affect the *Random* pool. They are
  // global across all AI seats because they describe which decks are worth
  // considering, not which deck ends up assigned — a concept that doesn't
  // vary per seat.
  const filteredDecks = useMemo(() => {
    // In cEDH mode, restrict the random pool to bracket-5 decks.
    const cedhFiltered = effectiveCedhMode ? filterByBracket(candidates, CEDH_BRACKET) : candidates;
    return cedhFiltered.filter((d) => {
      if (d.coveragePct != null && d.coveragePct < coverageFloor) return false;
      if (archetypeFilter !== "Any" && d.archetype && d.archetype !== archetypeFilter) {
        return false;
      }
      if (!effectiveCedhMode && bracketFilter.length > 0 && isCedhFormat) {
        if (d.bracket === null) return false;             // untagged excluded
        if (!bracketFilter.includes(d.bracket)) return false;
      }
      return true;
    });
  }, [candidates, coverageFloor, archetypeFilter, bracketFilter, isCedhFormat, effectiveCedhMode]);

  // Render exactly `opponentCount` panels regardless of how many slots the
  // store currently holds — the effect above will catch the store up on the
  // next tick, but the UI must not flash the wrong count in the meantime.
  const seatsToRender = useMemo(() => {
    const fallback = aiSeats[0];
    return Array.from({ length: opponentCount }, (_, i) =>
      aiSeats[i] ?? fallback ?? { difficulty: "Medium" as AIDifficulty, deckId: AI_DECK_RANDOM },
    );
  }, [aiSeats, opponentCount]);

  const isMulti = opponentCount > 1;

  // Track which seat panel is expanded in multi-AI mode. Single-AI mode
  // always renders the controls inline (no collapsing needed).
  const [expandedIndex, setExpandedIndex] = useState<number | null>(isMulti ? null : 0);

  // When switching between single and multi modes, reset the expansion state
  // so the UI starts in the canonical "single expanded / multi all collapsed"
  // configuration rather than inheriting a stale index.
  useEffect(() => {
    setExpandedIndex(isMulti ? null : 0);
  }, [isMulti]);

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <span className="text-[11px] font-semibold uppercase tracking-[0.14em] text-indigo-200">
          {isMulti ? t("aiOpponent.headingMulti", { count: opponentCount }) : t("aiOpponent.heading")}
        </span>
        {loading && <span className="text-[10px] text-slate-500">{t("aiOpponent.analyzingDecks")}</span>}
      </div>

      {/* Table-wide cEDH toggle. cEDH is a table property (every deck bracket 5),
          not a per-seat difficulty — enabling it makes all AI play cEDH without
          touching each opponent's remembered difficulty. Commander-only. */}
      {isCedhFormat && (
        <div className="flex items-center justify-between gap-3 rounded-lg border border-rose-500/25 bg-rose-500/5 px-3 py-2">
          <div className="flex min-w-0 flex-col">
            <span className="text-xs font-semibold text-rose-200">{t("aiOpponent.cedhToggle.label")}</span>
            <span className="text-[10px] text-slate-400">{t("aiOpponent.cedhToggle.hint")}</span>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={cedhMode}
            aria-label={t("aiOpponent.cedhToggle.label")}
            onClick={() => setCedhMode(!cedhMode)}
            className={`relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-rose-300/60 ${
              cedhMode ? "bg-rose-500" : "bg-white/15"
            }`}
          >
            <span
              className={`inline-block h-5 w-5 transform rounded-full bg-white transition-transform ${
                cedhMode ? "translate-x-5" : "translate-x-0.5"
              }`}
            />
          </button>
        </div>
      )}

      <div className="flex flex-col gap-1.5">
        {seatsToRender.map((seat, i) => (
          <AiSeatPanel
            key={i}
            index={i}
            seat={seat}
            cedhMode={effectiveCedhMode}
            candidates={candidates}
            filteredDecks={filteredDecks}
            expanded={!isMulti || expandedIndex === i}
            collapsible={isMulti}
            onToggle={() => setExpandedIndex((cur) => (cur === i ? null : i))}
            onDeckChange={(id) => setAiSeatDeckId(i, id)}
            onDifficultyChange={(d) => setAiSeatDifficulty(i, d)}
          />
        ))}
      </div>

      {!loading && candidates.length === 0 && (
        <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
          {t("aiOpponent.noLegalDecks")}
        </div>
      )}

      {error && (
        <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
          {t("aiOpponent.catalogUnavailable", { error })}
        </div>
      )}

      {/* Global pool filters — apply to every seat set to Random. */}
      <div className="mt-1 flex flex-col gap-3 rounded-lg border border-white/5 bg-black/20 px-3 py-2.5">
        <div className="text-[10px] font-semibold uppercase tracking-[0.14em] text-slate-500">
          {t("aiOpponent.randomPoolFilters")}
        </div>
        <label className="flex flex-col gap-1">
          <span className="text-xs text-slate-400">{t("aiOpponent.archetype")}</span>
          <SelectField
            wrapperClassName="w-full"
            value={archetypeFilter}
            onChange={(e) => setArchetypeFilter(e.target.value as AiArchetypeFilter)}
            className={`w-full rounded-lg border border-gray-700 bg-gray-800/60 px-2 py-1.5 text-sm font-medium ${archetypeAccent(
              archetypeFilter === "Any" ? null : (archetypeFilter as DeckArchetype),
            )}`}
          >
            {ARCHETYPE_OPTIONS.map((opt) => (
              <option key={opt} value={opt} className="text-white">
                {opt}
              </option>
            ))}
          </SelectField>
        </label>

        <label className="flex flex-col gap-1">
          <div className="flex items-center justify-between">
            <span className="text-xs text-slate-400">{t("aiOpponent.cardCoverage")}</span>
            <span className="text-sm font-medium text-white">{coverageFloor}%</span>
          </div>
          <input
            type="range"
            min={50}
            max={100}
            step={5}
            value={coverageFloor}
            onChange={(e) => setCoverageFloor(Number(e.target.value))}
            className="w-full"
          />
          <span className="text-[10px] text-slate-500">
            {t("aiOpponent.coverageThresholdHint")}
          </span>
        </label>

        {isCedhFormat && (
          <div className="flex flex-col gap-1">
            <span className="text-xs text-slate-400">{t("aiOpponent.bracket")}</span>
            <BracketFilter selected={bracketFilter} onChange={setBracketFilter} />
            <span className="text-[10px] text-slate-500">
              {t("aiOpponent.bracketHint")}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}

interface AiSeatPanelProps {
  index: number;
  seat: { difficulty: AIDifficulty; deckId: AiDeckSelection };
  /** Table-wide cEDH mode. When on, the per-seat difficulty is overridden by
   *  cEDH, so the dropdown is disabled and badged (the remembered value is kept
   *  for when cEDH is turned back off). */
  cedhMode: boolean;
  candidates: AiDeckCandidate[];
  filteredDecks: AiDeckCandidate[];
  expanded: boolean;
  collapsible: boolean;
  onToggle: () => void;
  onDeckChange: (name: AiDeckSelection) => void;
  onDifficultyChange: (d: AIDifficulty) => void;
}

function AiSeatPanel({
  index,
  seat,
  cedhMode,
  candidates,
  filteredDecks,
  expanded,
  collapsible,
  onToggle,
  onDeckChange,
  onDifficultyChange,
}: AiSeatPanelProps) {
  const { t } = useTranslation("menu");
  const isRandom = seat.deckId === AI_DECK_RANDOM;
  // When the user has pinned a deck, expose the full list so they can switch
  // to another pinned deck; otherwise scope to the filtered Random pool so
  // the "Random" summary count matches the options shown.
  const deckOptions = isRandom ? filteredDecks : candidates;
  const selectionValid = isRandom || deckOptions.some((d) => d.id === seat.deckId);
  const effectiveSelection: AiDeckSelection = selectionValid ? seat.deckId : AI_DECK_RANDOM;

  const sourceLabel = (candidate: AiDeckCandidate): string => {
    switch (candidate.source.type) {
      case "saved":
        return candidate.source.feedId ? t("aiOpponent.source.feed") : t("aiOpponent.source.user");
      case "feed":
        return candidate.source.feedId;
      case "precon":
        return t("aiOpponent.source.precon");
    }
  };

  const selectedCandidate = candidates.find((d) => d.id === seat.deckId);
  const summaryDeck = isRandom
    ? t("aiOpponent.deckRandomCount", { count: filteredDecks.length })
    : (selectedCandidate?.name ?? t("aiOpponent.deckRandom"));
  const summaryDifficulty = cedhMode
    ? t("aiOpponent.cedhToggle.badge")
    : t(`aiDifficulty.levels.${seat.difficulty}`);

  const body = (
    <div className="flex flex-col gap-2.5 px-3 pb-3 pt-1">
      <label className="flex flex-col gap-1">
        <span className="text-xs text-slate-400">{t("aiOpponent.deck")}</span>
        <SelectField
          wrapperClassName="w-full"
          value={effectiveSelection}
          onChange={(e) => onDeckChange(e.target.value as AiDeckSelection)}
          className="w-full rounded-lg border border-gray-700 bg-gray-800/60 px-2 py-1.5 text-sm text-white"
        >
          <option value={AI_DECK_RANDOM}>{t("aiOpponent.deckRandomCount", { count: filteredDecks.length })}</option>
          {deckOptions.map((d) => {
            const suffix = [sourceLabel(d), d.archetype, d.coveragePct != null ? `${d.coveragePct}%` : null]
              .filter(Boolean)
              .join(" · ");
            return (
              <option key={d.id} value={d.id}>
                {d.name}
                {suffix ? ` — ${suffix}` : ""}
              </option>
            );
          })}
        </SelectField>
      </label>

      <label className="flex flex-col gap-1">
        <span className="text-xs text-slate-400">{t("aiOpponent.difficulty")}</span>
        <div className="relative">
          <SelectField
            wrapperClassName="w-full"
            value={seat.difficulty}
            onChange={(e) => onDifficultyChange(e.target.value as AIDifficulty)}
            disabled={cedhMode}
            aria-disabled={cedhMode}
            className={`w-full rounded-lg border border-gray-700 bg-gray-800/60 px-2 py-1.5 text-sm text-white ${
              cedhMode ? "cursor-not-allowed opacity-50" : ""
            }`}
          >
            {AI_DIFFICULTIES.map((item) => (
              <option key={item.id} value={item.id}>
                {t(`aiDifficulty.levels.${item.id}`)}
              </option>
            ))}
          </SelectField>
          {cedhMode && (
            <span
              aria-label="cEDH"
              className="absolute -top-2 left-2 rounded bg-rose-500/80 px-1 py-0.5 text-[9px] font-bold uppercase tracking-wider text-white"
            >
              {t("aiOpponent.cedhToggle.badge")}
            </span>
          )}
        </div>
      </label>
    </div>
  );

  if (!collapsible) {
    return <div className="rounded-lg border border-white/8 bg-black/12">{body}</div>;
  }

  return (
    <div className="overflow-hidden rounded-lg border border-white/8 bg-black/12">
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={expanded}
        className="flex w-full items-center justify-between gap-2 px-3 py-2 text-left transition-colors hover:bg-white/4"
      >
        <div className="flex min-w-0 flex-col">
          <span className="text-xs font-semibold text-slate-200">{t("aiOpponent.opponentLabel", { number: index + 1 })}</span>
          <span className="truncate text-[11px] text-slate-400">
            {summaryDeck} · {summaryDifficulty}
          </span>
        </div>
        <Chevron expanded={expanded} />
      </button>
      {expanded && body}
    </div>
  );
}

function Chevron({ expanded }: { expanded: boolean }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 20 20"
      className={`h-4 w-4 flex-shrink-0 text-slate-500 transition-transform ${
        expanded ? "rotate-180" : ""
      }`}
      fill="currentColor"
    >
      <path
        fillRule="evenodd"
        d="M5.23 7.21a.75.75 0 0 1 1.06.02L10 11.06l3.71-3.83a.75.75 0 1 1 1.08 1.04l-4.25 4.39a.75.75 0 0 1-1.08 0L5.21 8.27a.75.75 0 0 1 .02-1.06Z"
        clipRule="evenodd"
      />
    </svg>
  );
}
