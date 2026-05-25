import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type { GameFormat, MatchType } from "../../adapter/types";
import { AI_DIFFICULTIES, type AIDifficulty } from "../../constants/ai";
import type { AiDeckCandidate } from "../../services/aiDeckCatalog";
import { useAiDeckCatalog } from "../../services/aiDeckCatalog";
import {
  AI_DECK_RANDOM,
  usePreferencesStore,
  type AiArchetypeFilter,
  type AiDeckSelection,
} from "../../stores/preferencesStore";
import type { DeckArchetype } from "../../services/engineRuntime";
import { BracketFilter } from "./BracketFilter";

interface Props {
  selectedFormat?: GameFormat;
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
  const setAiSeatDifficulty = usePreferencesStore((s) => s.setAiSeatDifficulty);
  const setAiSeatDeckId = usePreferencesStore((s) => s.setAiSeatDeckId);
  const ensureAiSeatCount = usePreferencesStore((s) => s.ensureAiSeatCount);
  const archetypeFilter = usePreferencesStore((s) => s.aiArchetypeFilter);
  const setArchetypeFilter = usePreferencesStore((s) => s.setAiArchetypeFilter);
  const coverageFloor = usePreferencesStore((s) => s.aiCoverageFloor);
  const setCoverageFloor = usePreferencesStore((s) => s.setAiCoverageFloor);
  const bracketFilter = usePreferencesStore((s) => s.aiBracketFilter);
  const setBracketFilter = usePreferencesStore((s) => s.setAiBracketFilter);

  // Keep the persisted seat list in sync with the setup page's player count.
  useEffect(() => {
    ensureAiSeatCount(opponentCount);
  }, [opponentCount, ensureAiSeatCount]);

  const { candidates, loading, error } = useAiDeckCatalog({ selectedFormat, selectedMatchType });

  useEffect(() => {
    onCandidateCountChange?.(loading ? null : candidates.length);
  }, [candidates.length, loading, onCandidateCountChange]);

  // The archetype + coverage filters only affect the *Random* pool. They are
  // global across all AI seats because they describe which decks are worth
  // considering, not which deck ends up assigned — a concept that doesn't
  // vary per seat.
  const filteredDecks = useMemo(() => {
    return candidates.filter((d) => {
      if (d.coveragePct != null && d.coveragePct < coverageFloor) return false;
      if (archetypeFilter !== "Any" && d.archetype && d.archetype !== archetypeFilter) {
        return false;
      }
      if (bracketFilter.length > 0 && selectedFormat === "Commander") {
        if (d.bracket === null) return false;             // untagged excluded
        if (!bracketFilter.includes(d.bracket)) return false;
      }
      return true;
    });
  }, [candidates, coverageFloor, archetypeFilter, bracketFilter, selectedFormat]);

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

      <div className="flex flex-col gap-1.5">
        {seatsToRender.map((seat, i) => (
          <AiSeatPanel
            key={i}
            index={i}
            seat={seat}
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
          <select
            value={archetypeFilter}
            onChange={(e) => setArchetypeFilter(e.target.value as AiArchetypeFilter)}
            className={`rounded-lg border border-gray-700 bg-gray-800/60 px-2 py-1.5 text-sm font-medium ${archetypeAccent(
              archetypeFilter === "Any" ? null : (archetypeFilter as DeckArchetype),
            )}`}
          >
            {ARCHETYPE_OPTIONS.map((opt) => (
              <option key={opt} value={opt} className="text-white">
                {opt}
              </option>
            ))}
          </select>
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

        {selectedFormat === "Commander" && (
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
  const summaryDifficulty = t(`aiDifficulty.levels.${seat.difficulty}`);

  const body = (
    <div className="flex flex-col gap-2.5 px-3 pb-3 pt-1">
      <label className="flex flex-col gap-1">
        <span className="text-xs text-slate-400">{t("aiOpponent.deck")}</span>
        <select
          value={effectiveSelection}
          onChange={(e) => onDeckChange(e.target.value as AiDeckSelection)}
          className="rounded-lg border border-gray-700 bg-gray-800/60 px-2 py-1.5 text-sm text-white"
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
        </select>
      </label>

      <label className="flex flex-col gap-1">
        <span className="text-xs text-slate-400">{t("aiOpponent.difficulty")}</span>
        <select
          value={seat.difficulty}
          onChange={(e) => onDifficultyChange(e.target.value as AIDifficulty)}
          className="rounded-lg border border-gray-700 bg-gray-800/60 px-2 py-1.5 text-sm text-white"
        >
          {AI_DIFFICULTIES.map((item) => (
            <option key={item.id} value={item.id}>
              {t(`aiDifficulty.levels.${item.id}`)}
            </option>
          ))}
        </select>
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
