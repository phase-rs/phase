import { useEffect, useMemo, useState } from "react";

import { useFeedDeckList } from "../../hooks/useFeedDeckList";
import type { FeedDeck } from "../../types/feed";
import type { FeedDeckMeta } from "../../hooks/useFeedDeckList";
import { AI_DIFFICULTIES, getAiDifficultyLabel, type AIDifficulty } from "../../constants/ai";
import {
  AI_DECK_RANDOM,
  usePreferencesStore,
  type AiArchetypeFilter,
  type AiDeckSelection,
} from "../../stores/preferencesStore";
import type { DeckArchetype } from "../../services/engineRuntime";

interface Props {
  format?: string;
  /** Number of AI opponents to configure (i.e. playerCount - 1). Defaults to 1
   *  so the component still renders sensibly when mounted outside the setup
   *  page's player-count context. */
  opponentCount?: number;
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

/** Ordinal suffix for a 1-based seat index. Used in multi-AI headers
 *  ("Opponent 1", "Opponent 2") to give each row a stable identity. */
function opponentLabel(index: number): string {
  return `Opponent ${index + 1}`;
}

export function AiOpponentConfig({ format, opponentCount = 1 }: Props) {
  const aiSeats = usePreferencesStore((s) => s.aiSeats);
  const setAiSeatDifficulty = usePreferencesStore((s) => s.setAiSeatDifficulty);
  const setAiSeatDeckName = usePreferencesStore((s) => s.setAiSeatDeckName);
  const ensureAiSeatCount = usePreferencesStore((s) => s.ensureAiSeatCount);
  const archetypeFilter = usePreferencesStore((s) => s.aiArchetypeFilter);
  const setArchetypeFilter = usePreferencesStore((s) => s.setAiArchetypeFilter);
  const coverageFloor = usePreferencesStore((s) => s.aiCoverageFloor);
  const setCoverageFloor = usePreferencesStore((s) => s.setAiCoverageFloor);

  // Keep the persisted seat list in sync with the setup page's player count.
  useEffect(() => {
    ensureAiSeatCount(opponentCount);
  }, [opponentCount, ensureAiSeatCount]);

  const { decks, meta, loading } = useFeedDeckList(format);

  // The archetype + coverage filters only affect the *Random* pool. They are
  // global across all AI seats because they describe which decks are worth
  // considering, not which deck ends up assigned — a concept that doesn't
  // vary per seat.
  const filteredDecks = useMemo(() => {
    return decks.filter((d) => {
      const m: FeedDeckMeta | undefined = meta.get(d.name);
      if (m?.coveragePct != null && m.coveragePct < coverageFloor) return false;
      if (archetypeFilter !== "Any" && m?.archetype && m.archetype !== archetypeFilter) {
        return false;
      }
      return true;
    });
  }, [decks, meta, coverageFloor, archetypeFilter]);

  // Render exactly `opponentCount` panels regardless of how many slots the
  // store currently holds — the effect above will catch the store up on the
  // next tick, but the UI must not flash the wrong count in the meantime.
  const seatsToRender = useMemo(() => {
    const fallback = aiSeats[0];
    return Array.from({ length: opponentCount }, (_, i) =>
      aiSeats[i] ?? fallback ?? { difficulty: "Medium" as AIDifficulty, deckName: AI_DECK_RANDOM },
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
          {isMulti ? `AI Opponents (${opponentCount})` : "AI Opponent"}
        </span>
        {loading && <span className="text-[10px] text-slate-500">Analyzing decks…</span>}
      </div>

      <div className="flex flex-col gap-1.5">
        {seatsToRender.map((seat, i) => (
          <AiSeatPanel
            key={i}
            index={i}
            seat={seat}
            decks={decks}
            meta={meta}
            filteredDecks={filteredDecks}
            expanded={!isMulti || expandedIndex === i}
            collapsible={isMulti}
            onToggle={() => setExpandedIndex((cur) => (cur === i ? null : i))}
            onDeckChange={(name) => setAiSeatDeckName(i, name)}
            onDifficultyChange={(d) => setAiSeatDifficulty(i, d)}
          />
        ))}
      </div>

      {/* Global pool filters — apply to every seat set to Random. */}
      <div className="mt-1 flex flex-col gap-3 rounded-lg border border-white/5 bg-black/20 px-3 py-2.5">
        <div className="text-[10px] font-semibold uppercase tracking-[0.14em] text-slate-500">
          Random Pool Filters
        </div>
        <label className="flex flex-col gap-1">
          <span className="text-xs text-slate-400">Archetype</span>
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
            <span className="text-xs text-slate-400">Card Coverage</span>
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
            Exclude decks below this engine-support threshold
          </span>
        </label>
      </div>
    </div>
  );
}

interface AiSeatPanelProps {
  index: number;
  seat: { difficulty: AIDifficulty; deckName: AiDeckSelection };
  decks: FeedDeck[];
  meta: Map<string, FeedDeckMeta>;
  filteredDecks: FeedDeck[];
  expanded: boolean;
  collapsible: boolean;
  onToggle: () => void;
  onDeckChange: (name: AiDeckSelection) => void;
  onDifficultyChange: (d: AIDifficulty) => void;
}

function AiSeatPanel({
  index,
  seat,
  decks,
  meta,
  filteredDecks,
  expanded,
  collapsible,
  onToggle,
  onDeckChange,
  onDifficultyChange,
}: AiSeatPanelProps) {
  const isRandom = seat.deckName === AI_DECK_RANDOM;
  // When the user has pinned a deck, expose the full list so they can switch
  // to another pinned deck; otherwise scope to the filtered Random pool so
  // the "Random" summary count matches the options shown.
  const deckOptions = isRandom ? filteredDecks : decks;
  const selectionValid = isRandom || deckOptions.some((d) => d.name === seat.deckName);
  const effectiveSelection: AiDeckSelection = selectionValid ? seat.deckName : AI_DECK_RANDOM;

  const summaryDeck = isRandom ? `Random (${filteredDecks.length})` : seat.deckName;
  const summaryDifficulty = getAiDifficultyLabel(seat.difficulty);

  const body = (
    <div className="flex flex-col gap-2.5 px-3 pb-3 pt-1">
      <label className="flex flex-col gap-1">
        <span className="text-xs text-slate-400">Deck</span>
        <select
          value={effectiveSelection}
          onChange={(e) => onDeckChange(e.target.value as AiDeckSelection)}
          className="rounded-lg border border-gray-700 bg-gray-800/60 px-2 py-1.5 text-sm text-white"
        >
          <option value={AI_DECK_RANDOM}>Random ({filteredDecks.length})</option>
          {deckOptions.map((d) => {
            const m = meta.get(d.name);
            const suffix = [m?.archetype, m?.coveragePct != null ? `${m.coveragePct}%` : null]
              .filter(Boolean)
              .join(" · ");
            return (
              <option key={d.name} value={d.name}>
                {d.name}
                {suffix ? ` — ${suffix}` : ""}
              </option>
            );
          })}
        </select>
      </label>

      <label className="flex flex-col gap-1">
        <span className="text-xs text-slate-400">Difficulty</span>
        <select
          value={seat.difficulty}
          onChange={(e) => onDifficultyChange(e.target.value as AIDifficulty)}
          className="rounded-lg border border-gray-700 bg-gray-800/60 px-2 py-1.5 text-sm text-white"
        >
          {AI_DIFFICULTIES.map((item) => (
            <option key={item.id} value={item.id}>
              {item.label}
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
          <span className="text-xs font-semibold text-slate-200">{opponentLabel(index)}</span>
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
