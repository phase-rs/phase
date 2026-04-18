import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";

import { useCardImage } from "../../hooks/useCardImage";

// Supported handlers are now derived from the coverage export, not a hardcoded list.
// See `extractHandlerUsage` below — a handler is listed iff the parser produces it
// on ≥1 fully supported card, which filters out stubs and dead API surface.

// --- Per-card coverage types ---

type ParseCategory = "keyword" | "ability" | "trigger" | "static" | "replacement" | "cost";

interface ParsedItem {
  category: ParseCategory;
  label: string;
  source_text?: string;
  supported: boolean;
  details?: [string, string][];
  children?: ParsedItem[];
}

interface GapDetail {
  handler: string;
  source_text?: string;
}

interface CardCoverageResult {
  card_name: string;
  set_code: string;
  supported: boolean;
  gap_details?: GapDetail[];
  gap_count?: number;
  oracle_text?: string;
  parse_details?: ParsedItem[];
  /** Set codes the card has been printed in (from MTGJSON `printings`). */
  printings?: string[];
}

interface GapFrequency {
  handler: string;
  total_count: number;
  single_gap_cards: number;
  single_gap_by_format: Record<string, number>;
  oracle_patterns?: OraclePattern[];
  independence_ratio?: number;
  co_occurrences?: CoOccurrence[];
}

interface OraclePattern {
  pattern: string;
  count: number;
  example_cards: string[];
}

interface CoOccurrence {
  handler: string;
  shared_cards: number;
}

interface GapBundle {
  handlers: string[];
  unlocked_cards: number;
  unlocked_by_format: Record<string, number>;
}

interface CoverageSummary {
  total_cards: number;
  supported_cards: number;
  coverage_pct: number;
  coverage_by_format?: Record<string, FormatCoverageSummary>;
  cards: CardCoverageResult[];
  top_gaps?: GapFrequency[];
  gap_bundles?: GapBundle[];
}

interface FormatCoverageSummary {
  total_cards: number;
  supported_cards: number;
  coverage_pct: number;
}

const MAX_VISIBLE_CARDS = 200;

type MainView = "card-coverage" | "by-set" | "gap-analysis" | "supported-handlers";
type HandlerTab = "effects" | "triggers" | "keywords" | "statics" | "replacements";
type StatusFilter = "all" | "supported" | "unsupported";
type SortMode = "name" | "gaps-desc" | "gaps-asc";
type FormatFilter = "all" | "standard" | "modern" | "pioneer" | "pauper" | "commander" | "legacy" | "vintage";

const FORMAT_LABELS: Record<FormatFilter, string> = {
  all: "All Formats",
  standard: "Standard",
  modern: "Modern",
  pioneer: "Pioneer",
  pauper: "Pauper",
  commander: "Commander",
  legacy: "Legacy",
  vintage: "Vintage",
};

export function CardCoverageDashboard() {
  const [mainView, setMainView] = useState<MainView>("card-coverage");

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-[14px] border border-white/10 bg-[#0b1020]/96 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md sm:rounded-[20px] lg:rounded-[24px]">
      {/* Header */}
      <div className="border-b border-white/10 px-4 py-4 sm:px-6 sm:py-5">
        <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
          Engine Tools
        </div>
        <h2 className="mt-1 text-lg font-semibold text-white sm:text-xl">Card Coverage</h2>
        <p className="mt-1 text-xs text-slate-400 sm:text-sm">
          Inspect implementation coverage and supported engine handlers.
        </p>
      </div>

      {/* Tab bar */}
      <div className="flex flex-wrap gap-1.5 border-b border-white/10 px-4 py-3 sm:gap-2 sm:px-6">
        {(["card-coverage", "by-set", "gap-analysis", "supported-handlers"] as const).map((view) => (
          <button
            key={view}
            onClick={() => setMainView(view)}
            className={`rounded-[16px] border px-3 py-1.5 text-xs font-semibold transition sm:px-4 sm:text-sm ${
              mainView === view
                ? "border-sky-400/60 bg-sky-500/14 text-sky-100"
                : "border-white/8 bg-black/20 text-slate-400 hover:border-white/14 hover:text-slate-100"
            }`}
          >
            {view === "card-coverage"
              ? "Card Coverage"
              : view === "by-set"
                ? "By Set"
                : view === "gap-analysis"
                  ? "Gap Analysis"
                  : "Supported Handlers"}
          </button>
        ))}
      </div>

      {/* Content */}
      {mainView === "card-coverage" ? (
        <CardCoverageView />
      ) : mainView === "by-set" ? (
        <BySetView />
      ) : mainView === "gap-analysis" ? (
        <GapAnalysisView />
      ) : (
        <SupportedHandlersView />
      )}
    </div>
  );
}

// --- Card Coverage View ---

function CardCoverageView() {
  const [coverage, setCoverage] = useState<CoverageSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedCard, setSelectedCard] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [sortMode, setSortMode] = useState<SortMode>("name");
  const [focusIndex, setFocusIndex] = useState(-1);
  const listRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    fetch(__COVERAGE_DATA_URL__)
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then((data: CoverageSummary) => setCoverage(data))
      .catch((e) => setError(e.message))
      .finally(() => setLoading(false));
  }, []);

  const hasActiveFilter = search.length >= 2 || statusFilter !== "all";

  const filteredCards = useMemo(() => {
    if (!coverage) return [];
    const lowerSearch = search.toLowerCase();
    const filtered = coverage.cards.filter((card) => {
      if (statusFilter === "supported" && !card.supported) return false;
      if (statusFilter === "unsupported" && card.supported) return false;
      if (lowerSearch && !card.card_name.toLowerCase().includes(lowerSearch)) return false;
      return true;
    });
    switch (sortMode) {
      case "name":
        filtered.sort((a, b) => a.card_name.localeCompare(b.card_name));
        break;
      case "gaps-desc":
        filtered.sort((a, b) => (b.gap_count ?? 0) - (a.gap_count ?? 0) || a.card_name.localeCompare(b.card_name));
        break;
      case "gaps-asc":
        filtered.sort((a, b) => (a.gap_count ?? 0) - (b.gap_count ?? 0) || a.card_name.localeCompare(b.card_name));
        break;
    }
    return filtered;
  }, [coverage, search, statusFilter, sortMode]);

  const activeCard = useMemo(() => {
    if (!selectedCard) return null;
    return filteredCards.find((_, i) => `${filteredCards[i].card_name}-${i}` === selectedCard) ?? null;
  }, [selectedCard, filteredCards]);

  // Keyboard navigation handler
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      const cards = filteredCards.slice(0, MAX_VISIBLE_CARDS);
      if (cards.length === 0) return;

      switch (e.key) {
        case "ArrowDown": {
          e.preventDefault();
          const next = Math.min(focusIndex + 1, cards.length - 1);
          setFocusIndex(next);
          setSelectedCard(`${cards[next].card_name}-${next}`);
          // Scroll into view
          const el = listRef.current?.children[next] as HTMLElement | undefined;
          el?.scrollIntoView({ block: "nearest" });
          break;
        }
        case "ArrowUp": {
          e.preventDefault();
          const prev = Math.max(focusIndex - 1, 0);
          setFocusIndex(prev);
          setSelectedCard(`${cards[prev].card_name}-${prev}`);
          const el = listRef.current?.children[prev] as HTMLElement | undefined;
          el?.scrollIntoView({ block: "nearest" });
          break;
        }
        case "Escape":
          e.preventDefault();
          setSelectedCard(null);
          setFocusIndex(-1);
          searchRef.current?.focus();
          break;
      }
    },
    [filteredCards, focusIndex],
  );

  if (loading) {
    return (
      <div className="flex flex-1 items-center justify-center p-8">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-white/20 border-t-sky-300" />
      </div>
    );
  }

  if (error || !coverage) {
    return (
      <div className="flex-1 p-8 text-center text-sm text-slate-400">
        <p className="mb-2">No coverage data available.</p>
        <p className="font-mono text-xs text-slate-500">
          Generate it with: cargo run --bin coverage-report -- /path/to/cards --all &gt; client/public/coverage-data.json
        </p>
      </div>
    );
  }

  if (coverage.total_cards === 0) {
    return (
      <div className="flex-1 p-8 text-center text-sm text-slate-400">
        <p className="mb-2">Coverage data is empty (0 cards analyzed).</p>
        <p className="font-mono text-xs text-slate-500">
          Run: cargo run --bin coverage-report -- /path/to/cards --all &gt; client/public/coverage-data.json
        </p>
      </div>
    );
  }

  const progressColor =
    coverage.coverage_pct > 70
      ? "from-emerald-600 to-emerald-400"
      : coverage.coverage_pct > 40
        ? "from-yellow-600 to-yellow-400"
        : "from-red-600 to-red-400";
  const visibleCards = filteredCards.slice(0, MAX_VISIBLE_CARDS);

  return (
    <>
      {/* Condensed summary bar */}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-white/10 px-4 py-3 sm:px-6">
        <div className="flex items-center gap-2 text-sm sm:gap-3">
          <span className="text-xs text-slate-300 sm:text-sm">
            {coverage.supported_cards.toLocaleString()} / {coverage.total_cards.toLocaleString()}
          </span>
          <div className="h-2 w-16 overflow-hidden rounded-full bg-black/30 sm:w-24">
            <div
              className={`h-full rounded-full bg-gradient-to-r ${progressColor}`}
              style={{ width: `${Math.min(coverage.coverage_pct, 100)}%` }}
            />
          </div>
          <span className="font-mono text-xs text-emerald-300 sm:text-sm">
            {coverage.coverage_pct.toFixed(1)}%
          </span>
        </div>
        {/* Format breakdown shown in bar chart below — no inline duplication */}
      </div>

      {/* Master-detail split */}
      <div className="flex min-h-0 flex-1" onKeyDown={handleKeyDown}>
        {/* Left panel: card list — hidden on mobile when a card is selected */}
        <div className={`flex min-h-0 w-full flex-col border-r border-white/10 md:w-80 md:shrink-0 ${activeCard ? "hidden md:flex" : "flex"}`}>
          {/* Search, sort & filter */}
          <div className="space-y-2 px-3 py-3">
            <div className="flex gap-2">
              <input
                ref={searchRef}
                type="text"
                placeholder="Search cards..."
                value={search}
                onChange={(e) => { setSearch(e.target.value); setFocusIndex(-1); }}
                className="min-w-0 flex-1 rounded-[12px] border border-white/10 bg-black/18 px-3 py-1.5 text-sm text-white placeholder-slate-500 outline-none focus:border-sky-400/40"
              />
              <select
                value={statusFilter}
                onChange={(e) => { setStatusFilter(e.target.value as StatusFilter); setFocusIndex(-1); }}
                className="rounded-[12px] border border-white/10 bg-black/18 px-2 py-1.5 text-xs text-white outline-none focus:border-sky-400/40"
              >
                <option value="all">All</option>
                <option value="supported">Supported</option>
                <option value="unsupported">Unsupported</option>
              </select>
            </div>
            <div className="flex gap-2">
              <select
                value={sortMode}
                onChange={(e) => setSortMode(e.target.value as SortMode)}
                className="min-w-0 flex-1 rounded-[12px] border border-white/10 bg-black/18 px-2 py-1.5 text-xs text-white outline-none focus:border-sky-400/40"
              >
                <option value="name">Sort: A-Z</option>
                <option value="gaps-desc">Sort: Most Gaps</option>
                <option value="gaps-asc">Sort: Fewest Gaps</option>
              </select>
            </div>
          </div>

          {/* Scrollable card list */}
          <div className="min-h-0 flex-1 overflow-y-auto" ref={listRef} tabIndex={-1}>
            <>
              {visibleCards.map((card, i) => {
                  const cardKey = `${card.card_name}-${i}`;
                  const isSelected = selectedCard === cardKey;
                  const isFocused = focusIndex === i;
                  return (
                    <button
                      key={cardKey}
                      onClick={() => {
                        setSelectedCard(isSelected ? null : cardKey);
                        setFocusIndex(isSelected ? -1 : i);
                      }}
                      className={`flex w-full items-center gap-2 px-3 py-2 text-left text-[13px] transition ${
                        isSelected
                          ? "bg-sky-500/10 text-white"
                          : isFocused
                            ? "bg-white/[0.05] text-white"
                            : "text-slate-300 hover:bg-white/[0.03]"
                      }`}
                    >
                      <span
                        className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                          card.supported ? "bg-emerald-400" : "bg-rose-400"
                        }`}
                      />
                      <span className="min-w-0 flex-1 truncate">{card.card_name}</span>
                      {!card.supported && (card.gap_count ?? 0) > 0 && (
                        <span className="shrink-0 text-[10px] tabular-nums text-rose-400/70">
                          {card.gap_count}
                        </span>
                      )}
                    </button>
                  );
                })}
                {filteredCards.length > MAX_VISIBLE_CARDS && (
                  <div className="px-3 py-2 text-center text-[11px] text-slate-600">
                    {MAX_VISIBLE_CARDS} of {filteredCards.length} shown
                  </div>
                )}
                {filteredCards.length === 0 && (
                  <div className="px-3 py-10 text-center text-xs text-slate-500">No matches</div>
                )}
            </>
          </div>

          {/* List footer */}
          <div className="border-t border-white/8 px-3 py-2 text-center text-[11px] text-slate-600">
            {hasActiveFilter
              ? `${Math.min(filteredCards.length, MAX_VISIBLE_CARDS)} of ${filteredCards.length.toLocaleString()} matches`
              : `${coverage.total_cards.toLocaleString()} cards`}
          </div>
        </div>

        {/* Right panel: card detail — full-width on mobile, flex-1 on md+ */}
        <div className={`min-h-0 min-w-0 flex-1 overflow-y-auto ${activeCard ? "block" : "hidden md:block"}`}>
          {activeCard ? (
            <CardParseDetail card={activeCard} onBack={() => { setSelectedCard(null); setFocusIndex(-1); }} />
          ) : (
            <DetailEmptyState coverage={coverage} />
          )}
        </div>
      </div>
    </>
  );
}

const FORMAT_DISPLAY_NAMES: Record<string, string> = {
  standard: "Standard",
  standardbrawl: "Std Brawl",
  pioneer: "Pioneer",
  modern: "Modern",
  legacy: "Legacy",
  vintage: "Vintage",
  commander: "Commander",
  brawl: "Brawl",
  historic: "Historic",
  pauper: "Pauper",
};

/** Summary view shown in the detail panel when no card is selected. */
function DetailEmptyState({ coverage }: { coverage: CoverageSummary }) {
  const formatCoverage = Object.entries(coverage.coverage_by_format ?? {}).filter(
    ([, summary]) => summary.total_cards > 0,
  );

  return (
    <div className="flex h-full flex-col items-center justify-center px-8 py-12">
      <div className="mb-6 text-center">
        <div className="text-sm text-slate-400">Select a card to inspect its parse breakdown</div>
        <div className="mt-1 text-xs text-slate-600">
          Use arrow keys to navigate, Escape to deselect
        </div>
      </div>

      {/* Format coverage bar chart */}
      {formatCoverage.length > 0 && (
        <div className="w-full max-w-md">
          <div className="mb-2 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
            Coverage by Format
          </div>
          <div className="space-y-1.5">
            {[...formatCoverage]
              .sort(([, a], [, b]) => b.coverage_pct - a.coverage_pct)
              .map(([format, summary]) => {
                const barColor =
                  summary.coverage_pct > 70
                    ? "from-emerald-600 to-emerald-400"
                    : summary.coverage_pct > 40
                      ? "from-yellow-600 to-yellow-400"
                      : "from-red-600 to-red-400";
                return (
                  <div key={format} className="flex items-center gap-2">
                    <span className="w-[5.5rem] shrink-0 text-right text-[11px] text-slate-500">
                      {FORMAT_DISPLAY_NAMES[format] ?? format}
                    </span>
                    <div className="relative h-4 min-w-0 flex-1 overflow-hidden rounded bg-black/30">
                      <div
                        className={`absolute inset-y-0 left-0 rounded bg-gradient-to-r ${barColor}`}
                        style={{ width: `${Math.min(summary.coverage_pct, 100)}%` }}
                      />
                      <span className="absolute inset-0 flex items-center justify-center font-mono text-[10px] font-medium text-white/80">
                        {summary.coverage_pct.toFixed(1)}%
                      </span>
                    </div>
                    <span className="w-[6.5rem] shrink-0 text-right font-mono text-[10px] text-slate-600">
                      {summary.supported_cards.toLocaleString()} / {summary.total_cards.toLocaleString()}
                    </span>
                  </div>
                );
              })}
          </div>
        </div>
      )}
    </div>
  );
}

// --- By Set View ---

const MIN_SET_CARDS = 20;
const MIN_SET_COVERAGE = 90;

interface SetCoverage {
  set_code: string;
  total: number;
  supported: number;
  pct: number;
  gap_cards: CardCoverageResult[];
}

/** Aggregate cards by set code. A card counts toward every set it was printed in. */
function aggregateBySet(cards: CardCoverageResult[]): SetCoverage[] {
  const totals = new Map<string, { total: number; supported: number; gaps: CardCoverageResult[] }>();
  for (const card of cards) {
    const printings = card.printings ?? [];
    for (const code of printings) {
      let entry = totals.get(code);
      if (!entry) {
        entry = { total: 0, supported: 0, gaps: [] };
        totals.set(code, entry);
      }
      entry.total += 1;
      if (card.supported) {
        entry.supported += 1;
      } else {
        entry.gaps.push(card);
      }
    }
  }
  return [...totals.entries()]
    .map(([set_code, v]) => ({
      set_code,
      total: v.total,
      supported: v.supported,
      pct: v.total > 0 ? (100 * v.supported) / v.total : 0,
      gap_cards: v.gaps,
    }))
    .filter((s) => s.total >= MIN_SET_CARDS && s.pct >= MIN_SET_COVERAGE)
    .sort((a, b) => b.pct - a.pct || b.total - a.total);
}

function BySetView() {
  const [coverage, setCoverage] = useState<CoverageSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [expandedSet, setExpandedSet] = useState<string | null>(null);
  const [selectedGapCard, setSelectedGapCard] = useState<string | null>(null);

  useEffect(() => {
    fetch(__COVERAGE_DATA_URL__)
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then((data: CoverageSummary) => setCoverage(data))
      .catch(() => setCoverage(null))
      .finally(() => setLoading(false));
  }, []);

  const sets = useMemo(() => (coverage ? aggregateBySet(coverage.cards) : []), [coverage]);

  if (loading) {
    return (
      <div className="flex flex-1 items-center justify-center p-8">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-white/20 border-t-sky-300" />
      </div>
    );
  }

  if (!coverage) {
    return (
      <div className="flex-1 p-8 text-center text-sm text-slate-400">
        No coverage data available.
      </div>
    );
  }

  const hasPrintings = coverage.cards.some((c) => (c.printings ?? []).length > 0);
  if (!hasPrintings) {
    return (
      <div className="flex-1 p-8 text-center text-sm text-slate-400">
        <p className="mb-2">Set membership data is not in this coverage export.</p>
        <p className="font-mono text-xs text-slate-500">
          Regenerate with: ./scripts/gen-card-data.sh
        </p>
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
      <div className="border-b border-white/10 px-4 py-3 text-xs text-slate-400 sm:px-6">
        Sets with &ge;{MIN_SET_CARDS} cards and &ge;{MIN_SET_COVERAGE}% fully supported. Expand a set to inspect remaining gaps.
        <span className="ml-2 text-slate-500">({sets.length} sets)</span>
      </div>

      <div className="flex-1 overflow-y-auto px-4 py-4 sm:px-6">
        <div className="space-y-1">
          {sets.map((s) => (
            <SetRow
              key={s.set_code}
              set={s}
              isExpanded={expandedSet === s.set_code}
              onToggle={() => {
                setExpandedSet(expandedSet === s.set_code ? null : s.set_code);
                setSelectedGapCard(null);
              }}
              selectedGapCard={selectedGapCard}
              onSelectGapCard={(name) =>
                setSelectedGapCard(selectedGapCard === name ? null : name)
              }
            />
          ))}
          {sets.length === 0 && (
            <div className="px-3 py-10 text-center text-xs text-slate-500">
              No sets meet the threshold yet.
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function SetRow({
  set,
  isExpanded,
  onToggle,
  selectedGapCard,
  onSelectGapCard,
}: {
  set: SetCoverage;
  isExpanded: boolean;
  onToggle: () => void;
  selectedGapCard: string | null;
  onSelectGapCard: (name: string) => void;
}) {
  const barColor =
    set.pct >= 98
      ? "from-emerald-500 to-emerald-300"
      : set.pct >= 95
        ? "from-emerald-600 to-emerald-400"
        : "from-teal-600 to-teal-400";

  const selectedCard =
    selectedGapCard != null
      ? set.gap_cards.find((c) => c.card_name === selectedGapCard) ?? null
      : null;

  return (
    <div>
      <button
        onClick={onToggle}
        className={`flex w-full items-center gap-3 rounded-[10px] border px-3 py-2 text-left text-[13px] transition ${
          isExpanded
            ? "border-sky-400/30 bg-sky-500/8"
            : "border-white/5 bg-black/12 hover:border-white/10"
        }`}
      >
        <span className={`text-[10px] transition ${isExpanded ? "rotate-90" : ""}`}>&#9654;</span>
        <span className="w-14 shrink-0 font-mono text-xs font-semibold uppercase tracking-wider text-slate-200">
          {set.set_code}
        </span>
        <div className="relative h-3 min-w-0 flex-1 overflow-hidden rounded bg-black/30">
          <div
            className={`absolute inset-y-0 left-0 rounded bg-gradient-to-r ${barColor}`}
            style={{ width: `${Math.min(set.pct, 100)}%` }}
          />
        </div>
        <span className="shrink-0 font-mono text-[11px] text-emerald-300/90">
          {set.pct.toFixed(1)}%
        </span>
        <span className="hidden shrink-0 font-mono text-[11px] text-slate-400 sm:inline">
          {set.supported}/{set.total}
        </span>
        {set.gap_cards.length > 0 && (
          <span className="shrink-0 rounded-full bg-rose-500/12 px-2 py-0.5 font-mono text-[10px] text-rose-300/90">
            {set.gap_cards.length} gap{set.gap_cards.length === 1 ? "" : "s"}
          </span>
        )}
      </button>

      {isExpanded && (
        <div className="ml-3 mt-1 border-l border-white/8 pl-3 pt-1 sm:ml-6 sm:pl-4">
          {set.gap_cards.length === 0 ? (
            <div className="py-2 text-[11px] text-slate-500">No unsupported cards in this set.</div>
          ) : (
            <>
              <div className="mb-2 text-[10px] font-semibold uppercase tracking-[0.14em] text-slate-500">
                Unsupported cards ({set.gap_cards.length}) — click one to inspect parse tree
              </div>
              <div className="flex flex-wrap gap-1.5">
                {set.gap_cards.map((card) => {
                  const active = selectedGapCard === card.card_name;
                  return (
                    <button
                      key={card.card_name}
                      onClick={() => onSelectGapCard(card.card_name)}
                      className={`rounded-[6px] border px-2 py-1 text-[11px] transition ${
                        active
                          ? "border-sky-400/60 bg-sky-500/16 text-sky-100"
                          : "border-rose-400/20 bg-rose-500/8 text-rose-200/90 hover:border-rose-400/40"
                      }`}
                      title={
                        (card.gap_details ?? [])
                          .map((g) => g.handler)
                          .join(", ") || "no gap details"
                      }
                    >
                      {card.card_name}
                      {(card.gap_count ?? 0) > 0 && (
                        <span className="ml-1.5 text-[9px] text-rose-300/70">×{card.gap_count}</span>
                      )}
                    </button>
                  );
                })}
              </div>
              {selectedCard && (
                <div className="mt-3 rounded-[10px] border border-white/10 bg-black/30">
                  <CardParseDetail card={selectedCard} />
                </div>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

// --- Gap Analysis View ---

function GapAnalysisView() {
  const [coverage, setCoverage] = useState<CoverageSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [expandedGap, setExpandedGap] = useState<string | null>(null);
  const [formatFilter, setFormatFilter] = useState<FormatFilter>("all");

  useEffect(() => {
    fetch(__COVERAGE_DATA_URL__)
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then((data: CoverageSummary) => setCoverage(data))
      .catch(() => setCoverage(null))
      .finally(() => setLoading(false));
  }, []);

  if (loading) {
    return (
      <div className="flex flex-1 items-center justify-center p-8">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-white/20 border-t-sky-300" />
      </div>
    );
  }

  if (!coverage?.top_gaps?.length) {
    return (
      <div className="flex-1 p-8 text-center text-sm text-slate-400">
        No gap analysis data available.
      </div>
    );
  }

  const filteredGaps = coverage.top_gaps.map((gap) => {
    if (formatFilter === "all") return gap;
    const formatCount = gap.single_gap_by_format[formatFilter] ?? 0;
    return { ...gap, single_gap_cards: formatCount };
  });

  const bundles = coverage.gap_bundles ?? [];
  const twoBundles = bundles.filter((b) => b.handlers.length === 2);
  const threeBundles = bundles.filter((b) => b.handlers.length === 3);

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
      {/* Format filter bar */}
      <div className="flex items-center gap-2 border-b border-white/10 px-4 py-3 sm:px-6">
        <span className="text-xs text-slate-500">Filter by format:</span>
        <select
          value={formatFilter}
          onChange={(e) => setFormatFilter(e.target.value as FormatFilter)}
          className="rounded-[12px] border border-white/10 bg-black/18 px-3 py-1.5 text-xs text-white outline-none focus:border-sky-400/40"
        >
          {Object.entries(FORMAT_LABELS).map(([key, label]) => (
            <option key={key} value={key}>{label}</option>
          ))}
        </select>
      </div>

      <div className="flex-1 overflow-y-auto px-4 py-4 sm:px-6">
        {/* Top gaps */}
        <div className="mb-6">
          <div className="mb-3 flex items-center justify-between">
            <div className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
              Top Gaps by Impact (Top 50)
            </div>
            <CopyButton
              text={filteredGaps.map((g) => `${g.handler}\t${g.total_count}\t${g.single_gap_cards}`).join("\n")}
              label="Copy as TSV"
            />
          </div>
          <div className="space-y-1">
            {filteredGaps.map((gap) => (
              <GapRow
                key={gap.handler}
                gap={gap}
                isExpanded={expandedGap === gap.handler}
                onToggle={() => setExpandedGap(expandedGap === gap.handler ? null : gap.handler)}
                formatFilter={formatFilter}
              />
            ))}
          </div>
        </div>

        {/* 2-Gap Bundles */}
        {twoBundles.length > 0 && (
          <div className="mb-6">
            <div className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
              2-Gap Bundles (implement both to unlock cards)
            </div>
            <div className="space-y-1">
              {twoBundles.slice(0, 15).map((bundle, i) => (
                <BundleRow key={i} bundle={bundle} formatFilter={formatFilter} />
              ))}
            </div>
          </div>
        )}

        {/* 3-Gap Bundles */}
        {threeBundles.length > 0 && (
          <div className="mb-6">
            <div className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
              3-Gap Bundles (implement all three to unlock cards)
            </div>
            <div className="space-y-1">
              {threeBundles.slice(0, 10).map((bundle, i) => (
                <BundleRow key={i} bundle={bundle} formatFilter={formatFilter} />
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function GapRow({
  gap,
  isExpanded,
  onToggle,
  formatFilter,
}: {
  gap: GapFrequency;
  isExpanded: boolean;
  onToggle: () => void;
  formatFilter: FormatFilter;
}) {
  const ratioStr = gap.independence_ratio != null
    ? `${(gap.independence_ratio * 100).toFixed(0)}%`
    : null;

  return (
    <div>
      <button
        onClick={onToggle}
        className={`flex w-full items-center gap-3 rounded-[10px] border px-3 py-2 text-left text-[13px] transition ${
          isExpanded
            ? "border-sky-400/30 bg-sky-500/8"
            : "border-white/5 bg-black/12 hover:border-white/10"
        }`}
      >
        <span className={`text-[10px] transition ${isExpanded ? "rotate-90" : ""}`}>&#9654;</span>
        <span className="min-w-0 flex-1 font-medium text-slate-300">{gap.handler}</span>
        <span className="hidden shrink-0 font-mono text-xs text-amber-300/80 sm:inline">{gap.total_count} total</span>
        {gap.single_gap_cards > 0 && (
          <span className="shrink-0 font-mono text-xs text-emerald-300/80">
            {gap.single_gap_cards} unlock
          </span>
        )}
        {ratioStr && (
          <span className="hidden shrink-0 rounded-full bg-sky-500/12 px-2 py-0.5 text-[10px] text-sky-300 sm:inline">
            {ratioStr} ind
          </span>
        )}
      </button>

      {isExpanded && (
        <div className="ml-3 mt-1 space-y-2 border-l border-white/8 pl-3 pt-1 sm:ml-6 sm:pl-4">
          {/* Oracle patterns */}
          {gap.oracle_patterns && gap.oracle_patterns.length > 0 && (
            <div>
              <div className="mb-1 text-[10px] font-semibold uppercase tracking-[0.14em] text-slate-500">
                Oracle Patterns
              </div>
              <div className="space-y-0.5">
                {gap.oracle_patterns.slice(0, 10).map((pat, i) => (
                  <div key={i} className="flex items-start gap-2 text-[12px]">
                    <span className="shrink-0 font-mono text-amber-300/60">&times;{pat.count}</span>
                    <span className="min-w-0 flex-1 font-mono text-slate-400">
                      &laquo;{pat.pattern}&raquo;
                    </span>
                    {pat.example_cards.length > 0 && (
                      <span className="shrink-0 truncate text-[11px] text-slate-600">
                        e.g. {pat.example_cards[0]}
                      </span>
                    )}
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* Co-occurrences */}
          {gap.co_occurrences && gap.co_occurrences.length > 0 && (
            <div>
              <div className="mb-1 text-[10px] font-semibold uppercase tracking-[0.14em] text-slate-500">
                Co-occurring Gaps
              </div>
              <div className="flex flex-wrap gap-1">
                {gap.co_occurrences.slice(0, 8).map((co) => (
                  <span
                    key={co.handler}
                    className="rounded-[6px] border border-white/6 bg-black/20 px-2 py-0.5 text-[11px] text-slate-400"
                  >
                    {co.handler} <span className="text-slate-600">({co.shared_cards})</span>
                  </span>
                ))}
              </div>
            </div>
          )}

          {/* Format breakdown */}
          {formatFilter === "all" && Object.keys(gap.single_gap_by_format).length > 0 && (
            <div>
              <div className="mb-1 text-[10px] font-semibold uppercase tracking-[0.14em] text-slate-500">
                Single-Gap Unlock by Format
              </div>
              <div className="flex flex-wrap gap-1">
                {Object.entries(gap.single_gap_by_format).map(([fmt, count]) => (
                  <span
                    key={fmt}
                    className="rounded-[6px] border border-white/6 bg-black/20 px-2 py-0.5 text-[11px] text-slate-400"
                  >
                    <span className="uppercase">{fmt.slice(0, 3)}</span>:{count}
                  </span>
                ))}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function BundleRow({
  bundle,
  formatFilter,
}: {
  bundle: GapBundle;
  formatFilter: FormatFilter;
}) {
  const count = formatFilter === "all"
    ? bundle.unlocked_cards
    : (bundle.unlocked_by_format[formatFilter] ?? 0);

  if (count === 0) return null;

  return (
    <div className="flex items-center gap-3 rounded-[10px] border border-white/5 bg-black/12 px-3 py-2 text-[13px]">
      <div className="flex min-w-0 flex-1 flex-wrap gap-1">
        {bundle.handlers.map((h) => (
          <span
            key={h}
            className="rounded-[6px] border border-amber-400/20 bg-amber-500/8 px-2 py-0.5 text-[11px] text-amber-300"
          >
            {h}
          </span>
        ))}
      </div>
      <span className="shrink-0 font-mono text-xs text-emerald-300/80">
        {count} cards
      </span>
    </div>
  );
}

// --- Parse detail components ---

const CATEGORY_LABELS: Record<ParseCategory, string> = {
  keyword: "Keyword",
  ability: "Ability",
  trigger: "Trigger",
  static: "Static",
  replacement: "Replacement",
  cost: "Cost",
};

const CATEGORY_COLORS: Record<ParseCategory, string> = {
  keyword: "text-violet-300",
  ability: "text-sky-300",
  trigger: "text-amber-300",
  static: "text-teal-300",
  replacement: "text-orange-300",
  cost: "text-rose-300",
};

const CATEGORY_BG_COLORS: Record<ParseCategory, string> = {
  keyword: "bg-violet-400/20",
  ability: "bg-sky-400/20",
  trigger: "bg-amber-400/20",
  static: "bg-teal-400/20",
  replacement: "bg-orange-400/20",
  cost: "bg-rose-400/20",
};

const CATEGORY_RING_COLORS: Record<ParseCategory, string> = {
  keyword: "ring-violet-400/40",
  ability: "ring-sky-400/40",
  trigger: "ring-amber-400/40",
  static: "ring-teal-400/40",
  replacement: "ring-orange-400/40",
  cost: "ring-rose-400/40",
};

const CATEGORY_UNDERLINE_COLORS: Record<ParseCategory, string> = {
  keyword: "border-violet-400/60",
  ability: "border-sky-400/60",
  trigger: "border-amber-400/60",
  static: "border-teal-400/60",
  replacement: "border-orange-400/60",
  cost: "border-rose-400/60",
};

const CATEGORY_BORDER_COLORS: Record<ParseCategory, string> = {
  keyword: "border-l-violet-400/60",
  ability: "border-l-sky-400/60",
  trigger: "border-l-amber-400/60",
  static: "border-l-teal-400/60",
  replacement: "border-l-orange-400/60",
  cost: "border-l-rose-400/60",
};

const ABILITY_KIND_ICONS: Record<string, string> = {
  trigger: "\u26A1",
  activated: "\u2699",
  ability: "\u2726",
  static: "\uD83D\uDEE1",
  replacement: "\u21BA",
  keyword: "\uD83C\uDFF7",
  cost: "\u2726",
};

interface OracleLineData {
  line: string;
  segments: TextSegment[];
  matchedItems: ParsedItem[];
}

/** Maps parse items to oracle text lines using a three-pass strategy. */
function mapItemsToLines(
  oracleText: string,
  items: ParsedItem[],
  indexed: IndexedItem[],
): { lineData: OracleLineData[]; unmatchedItems: ParsedItem[] } {
  const lines = oracleText.split("\n");
  const lineItems: ParsedItem[][] = lines.map(() => []);
  const matched = new Set<ParsedItem>();

  // Pass 1: source_text containment with coverage scoring
  for (const item of items) {
    if (!item.source_text) continue;
    const lowerSource = item.source_text.toLowerCase();
    let bestLine = -1;
    let bestScore = -1;
    for (let i = 0; i < lines.length; i++) {
      const lowerLine = lines[i].toLowerCase();
      if (lowerLine.includes(lowerSource)) {
        const score = lowerSource.length / lowerLine.length;
        if (score > bestScore) {
          bestScore = score;
          bestLine = i;
        }
      }
    }
    // Fallback for multi-line source_text: match first line
    if (bestLine === -1 && lowerSource.includes("\n")) {
      const firstSourceLine = lowerSource.split("\n")[0];
      for (let i = 0; i < lines.length; i++) {
        if (lines[i].toLowerCase().includes(firstSourceLine)) {
          bestLine = i;
          break;
        }
      }
    }
    if (bestLine !== -1) {
      lineItems[bestLine].push(item);
      matched.add(item);
    }
  }

  // Pass 2: keyword label whole-word matching
  for (const item of items) {
    if (matched.has(item)) continue;
    if (item.category !== "keyword") continue;
    const lowerLabel = item.label.toLowerCase();
    const wordRegex = new RegExp(`\\b${lowerLabel.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`, "i");
    for (let i = 0; i < lines.length; i++) {
      if (wordRegex.test(lines[i])) {
        lineItems[i].push(item);
        matched.add(item);
        break;
      }
    }
  }

  // Pass 3: unmatched fallback
  const unmatchedItems = items.filter((item) => !matched.has(item));

  const lineData: OracleLineData[] = lines.map((line, i) => ({
    line,
    segments: annotateOracleLine(line, indexed),
    matchedItems: lineItems[i],
  }));

  return { lineData, unmatchedItems };
}

/** A flattened parse item with a stable ID for hover linking. */
interface IndexedItem {
  id: string;
  item: ParsedItem;
  /** The text to match in oracle text: explicit source_text, or keyword label. */
  matchText: string | null;
}

/** Flatten the parse tree into a list of indexed items for hover matching. */
function flattenParseItems(items: ParsedItem[], prefix = ""): IndexedItem[] {
  return items.flatMap((item, i) => {
    const id = prefix ? `${prefix}-${i}` : `${i}`;
    const matchText =
      item.source_text ??
      (item.category === "keyword" ? item.label : null);
    const self: IndexedItem = { id, item, matchText };
    const children = item.children?.length ? flattenParseItems(item.children, id) : [];
    return [self, ...children];
  });
}

/** A segment of oracle text: either plain or matched to a parse item. */
interface TextSegment {
  text: string;
  itemId: string | null;
  item: ParsedItem | null;
}

/** Build annotated text segments for a single oracle text line. */
function annotateOracleLine(line: string, indexed: IndexedItem[]): TextSegment[] {
  // Find all matches of indexed items within the line
  const lowerLine = line.toLowerCase();
  const matches = indexed.flatMap((entry) => {
    if (!entry.matchText) return [];
    const idx = lowerLine.indexOf(entry.matchText.toLowerCase());
    return idx !== -1 ? [{ start: idx, end: idx + entry.matchText.length, entry }] : [];
  });

  if (matches.length === 0) {
    return [{ text: line, itemId: null, item: null }];
  }

  // Sort by start position, prefer longer matches for ties
  matches.sort((a, b) => a.start - b.start || b.end - a.end);

  // Remove overlaps (greedy: keep first/longest)
  const resolved = matches.reduce<{ kept: typeof matches; lastEnd: number }>(
    (acc, m) => {
      if (m.start >= acc.lastEnd) {
        acc.kept.push(m);
        acc.lastEnd = m.end;
      }
      return acc;
    },
    { kept: [], lastEnd: 0 },
  ).kept;

  // Build segments by walking resolved matches and filling gaps with plain text
  const { segments, cursor } = resolved.reduce<{ segments: TextSegment[]; cursor: number }>(
    (acc, m) => {
      if (m.start > acc.cursor) {
        acc.segments.push({ text: line.slice(acc.cursor, m.start), itemId: null, item: null });
      }
      acc.segments.push({
        text: line.slice(m.start, m.end),
        itemId: m.entry.id,
        item: m.entry.item,
      });
      return { segments: acc.segments, cursor: m.end };
    },
    { segments: [], cursor: 0 },
  );
  if (cursor < line.length) {
    segments.push({ text: line.slice(cursor), itemId: null, item: null });
  }

  return segments;
}

function CardParseDetail({ card, onBack }: { card: CardCoverageResult; onBack?: () => void }) {
  const [hoveredId, setHoveredId] = useState<string | null>(null);
  const { src: cardImageSrc, isLoading: cardImageLoading } = useCardImage(card.card_name, { size: "normal" });

  const onHover = useCallback((id: string | null) => setHoveredId(id), []);

  const indexed = useMemo(
    () => flattenParseItems(card.parse_details ?? []),
    [card.parse_details],
  );

  const items = useMemo(() => card.parse_details ?? [], [card.parse_details]);

  const { lineData, unmatchedItems } = useMemo(() => {
    if (!card.oracle_text) return { lineData: [], unmatchedItems: items };
    return mapItemsToLines(card.oracle_text, items, indexed);
  }, [card.oracle_text, items, indexed]);

  const totalItems = items.length;
  const [expandedLines, setExpandedLines] = useState<Set<number>>(() => {
    if (totalItems <= 6) {
      return new Set(lineData.map((_, i) => i));
    }
    return new Set<number>();
  });

  const toggleLine = useCallback((idx: number) => {
    setExpandedLines((prev) => {
      const next = new Set(prev);
      if (next.has(idx)) {
        next.delete(idx);
      } else {
        next.add(idx);
      }
      return next;
    });
  }, []);

  return (
    <div className="px-4 py-4 sm:px-6">
      {/* Mobile back button */}
      {onBack && (
        <button
          onClick={onBack}
          className="mb-3 flex items-center gap-1.5 text-xs text-slate-400 transition hover:text-slate-200 md:hidden"
        >
          <span className="text-sm">&larr;</span>
          Back to list
        </button>
      )}

      {/* Card header with image */}
      <div className="mb-5 flex flex-col gap-4 sm:flex-row sm:gap-5">
        {/* Card image */}
        <div className="mx-auto w-[160px] shrink-0 sm:mx-0 sm:w-[200px]">
          {cardImageLoading || !cardImageSrc ? (
            <div className="aspect-[488/680] w-full animate-pulse rounded-lg bg-white/5" />
          ) : (
            <img
              src={cardImageSrc}
              alt={card.card_name}
              className="w-full rounded-lg shadow-[0_8px_32px_rgba(0,0,0,0.5)]"
              draggable={false}
            />
          )}
        </div>

        {/* Card name + status */}
        <div className="min-w-0 flex-1">
          <div className="mb-3 flex flex-wrap items-center gap-2 sm:gap-3">
            <h3 className="text-base font-semibold text-white">{card.card_name}</h3>
            {card.supported ? (
              <span className="rounded-full border border-emerald-400/30 bg-emerald-500/12 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-[0.14em] text-emerald-300">
                Supported
              </span>
            ) : (
              <span className="rounded-full border border-rose-400/30 bg-rose-500/12 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-[0.14em] text-rose-300">
                Unsupported
              </span>
            )}
            {card.oracle_text && (
              <CopyButton text={card.oracle_text} label="Copy Oracle" className="sm:ml-auto" />
            )}
          </div>
        </div>
      </div>

      {/* Oracle-centric unified view */}
      {lineData.length > 0 ? (
        <div className="space-y-1">
          {lineData.map((ld, i) => (
            <OracleLineSection
              key={i}
              lineData={ld}
              lineIndex={i}
              isExpanded={expandedLines.has(i)}
              onToggle={() => toggleLine(i)}
              hoveredId={hoveredId}
              onHover={onHover}
              indexed={indexed}
            />
          ))}
        </div>
      ) : totalItems === 0 ? (
        <div className="text-xs text-slate-500">No parsed items (vanilla card).</div>
      ) : null}

      {/* Unmatched items fallback */}
      {unmatchedItems.length > 0 && (
        <div className="mt-4">
          <div className="mb-2 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
            Additional Parse Items
          </div>
          <div className="space-y-0.5">
            {unmatchedItems.map((item, i) => (
              <ParseNode
                key={i}
                item={item}
                depth={0}
                isLast={i === unmatchedItems.length - 1}
                hoveredId={hoveredId}
                onHover={onHover}
                indexed={indexed}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function OracleLineSection({
  lineData,
  lineIndex,
  isExpanded,
  onToggle,
  hoveredId,
  onHover,
  indexed,
}: {
  lineData: OracleLineData;
  lineIndex: number;
  isExpanded: boolean;
  onToggle: () => void;
  hoveredId: string | null;
  onHover: (id: string | null) => void;
  indexed: IndexedItem[];
}) {
  const hasItems = lineData.matchedItems.length > 0;
  const hasUnsupported = lineData.matchedItems.some((item) => !item.supported);
  const primaryCategory = lineData.matchedItems[0]?.category;
  const borderColor = hasUnsupported
    ? "border-l-rose-400/60"
    : primaryCategory
      ? CATEGORY_BORDER_COLORS[primaryCategory]
      : "border-l-white/10";

  return (
    <div className={`border-l-2 ${borderColor} pl-3`}>
      {/* Oracle text line with optional chevron */}
      <div
        className={`flex items-start gap-1.5 py-1 ${hasItems ? "cursor-pointer" : ""}`}
        onClick={hasItems ? onToggle : undefined}
      >
        {hasItems ? (
          <span className={`mt-0.5 text-[10px] text-slate-500 transition-transform duration-150 ${isExpanded ? "rotate-90" : ""}`}>
            &#9654;
          </span>
        ) : (
          <span className="mt-0.5 w-[10px]" />
        )}
        <div className="min-w-0 flex-1 font-mono text-xs leading-[1.8] text-slate-300">
          {lineData.segments.map((seg, segIdx) =>
            seg.itemId ? (
              <span
                key={segIdx}
                className={`cursor-pointer rounded-[3px] transition-colors duration-100 ${
                  hoveredId === seg.itemId
                    ? `${CATEGORY_BG_COLORS[seg.item!.category]} border-b ${CATEGORY_UNDERLINE_COLORS[seg.item!.category]}`
                    : `border-b border-dashed ${seg.item!.supported ? `${CATEGORY_UNDERLINE_COLORS[seg.item!.category]} text-slate-200` : "border-rose-400/50 text-rose-300"}`
                }`}
                onMouseEnter={() => onHover(seg.itemId)}
                onMouseLeave={() => onHover(null)}
              >
                {seg.text}
              </span>
            ) : (
              <span key={segIdx}>{seg.text}</span>
            ),
          )}
        </div>
        {hasItems && (
          <span className={`mt-1 h-1.5 w-1.5 shrink-0 rounded-full ${hasUnsupported ? "bg-rose-400" : "bg-emerald-400"}`} />
        )}
      </div>

      {/* Expandable parse nodes */}
      <AnimatePresence initial={false}>
        {isExpanded && hasItems && (
          <motion.div
            key={`line-${lineIndex}`}
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2, ease: "easeInOut" }}
            className="overflow-hidden"
          >
            <div className="ml-3 space-y-0.5 pb-1">
              {lineData.matchedItems.map((item, i) => (
                <ParseNode
                  key={i}
                  item={item}
                  depth={0}
                  isLast={i === lineData.matchedItems.length - 1}
                  hoveredId={hoveredId}
                  onHover={onHover}
                  indexed={indexed}
                />
              ))}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

function ParseNode({
  item,
  depth,
  isLast,
  parentCategory,
  hoveredId,
  onHover,
  indexed,
}: {
  item: ParsedItem;
  depth: number;
  isLast: boolean;
  parentCategory?: ParseCategory;
  hoveredId: string | null;
  onHover: (id: string | null) => void;
  indexed: IndexedItem[];
}) {
  const hasChildren = (item.children?.length ?? 0) > 0;
  const labelColor = item.supported ? "text-slate-200" : "text-rose-300";
  const icon = ABILITY_KIND_ICONS[item.category] ?? "\u2726";

  const matchedEntry = indexed.find((e) => e.item === item);
  const itemId = matchedEntry?.id ?? null;
  const isHighlighted = itemId !== null && hoveredId === itemId;

  return (
    <div className="flex">
      {/* Tree connector */}
      {depth > 0 && (
        <div className="flex w-4 shrink-0 flex-col items-center">
          <div className={`w-px flex-1 ${isLast ? "" : "bg-white/10"}`} />
        </div>
      )}
      <div className="min-w-0 flex-1">
        <div
          className={`flex items-start gap-1.5 rounded-[6px] px-1.5 py-0.5 transition-colors duration-100 ${
            isHighlighted
              ? `${CATEGORY_BG_COLORS[item.category]} ring-1 ring-inset ${CATEGORY_RING_COLORS[item.category]}`
              : "hover:bg-white/[0.03]"
          }`}
          onMouseEnter={() => itemId && onHover(itemId)}
          onMouseLeave={() => onHover(null)}
        >
          {/* Tree connector arm */}
          {depth > 0 && (
            <span className="mt-1 text-white/10">{isLast ? "\u2514" : "\u251C"}</span>
          )}
          <span className="mt-px text-[10px]">{icon}</span>
          <div className="min-w-0 flex-1">
            <div className="flex items-baseline gap-1.5">
              <span className={`text-xs font-medium ${labelColor}`}>{item.label}</span>
              {depth > 0 && parentCategory != null && item.category !== parentCategory && (
                <span className={`text-[10px] ${CATEGORY_COLORS[item.category]}`}>
                  {CATEGORY_LABELS[item.category]}
                </span>
              )}
              {!item.supported && (
                <span className="text-[9px] font-semibold uppercase tracking-[0.12em] text-rose-400">
                  unsupported
                </span>
              )}
            </div>
            {item.details && item.details.length > 0 && (
              <div className="mt-0.5 flex flex-wrap gap-1">
                {item.details.map(([key, value], i) => (
                  <span
                    key={i}
                    className={`inline-flex items-baseline gap-1 rounded-[4px] px-1.5 py-0.5 text-[10px] leading-tight ${CATEGORY_BG_COLORS[item.category]}`}
                  >
                    <span className="text-slate-500">{key}</span>
                    <span className={CATEGORY_COLORS[item.category]}>{value}</span>
                  </span>
                ))}
              </div>
            )}
          </div>
        </div>
        {hasChildren && (
          <div className={`${depth === 0 ? "ml-2 border-l border-white/10" : ""}`}>
            {item.children!.map((child, i) => (
              <ParseNode
                key={i}
                item={child}
                depth={depth + 1}
                isLast={i === item.children!.length - 1}
                parentCategory={item.category}
                hoveredId={hoveredId}
                onHover={onHover}
                indexed={indexed}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// --- Copy button utility ---

function CopyButton({ text, label, className = "" }: { text: string; label: string; className?: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }, [text]);

  return (
    <button
      onClick={handleCopy}
      className={`rounded-[8px] border border-white/8 bg-black/20 px-2 py-0.5 text-[10px] text-slate-500 transition hover:border-white/14 hover:text-slate-300 ${className}`}
    >
      {copied ? "Copied!" : label}
    </button>
  );
}

// --- Supported Handlers View (derived from engine-produced parse data) ---

interface HandlerUsage {
  label: string;
  count: number;
}

/** Category on a ParsedItem matches the HandlerTab buckets (except "effects" == "ability"). */
const HANDLER_TAB_TO_CATEGORIES: Record<HandlerTab, ParseCategory[]> = {
  effects: ["ability"],
  triggers: ["trigger"],
  keywords: ["keyword"],
  statics: ["static"],
  replacements: ["replacement"],
};

/** Walk the parse tree, counting handlers by category from fully supported cards only.
 *  A handler present on a supported card means: parser produced it AND the card is considered
 *  supported end-to-end — i.e., the resolver has a real path for it. Handlers that only appear
 *  on unsupported cards or never appear are filtered out as stubs/unused. */
function extractHandlerUsage(
  cards: CardCoverageResult[],
): Record<HandlerTab, HandlerUsage[]> {
  const perCategory: Record<ParseCategory, Map<string, number>> = {
    ability: new Map(),
    trigger: new Map(),
    keyword: new Map(),
    static: new Map(),
    replacement: new Map(),
    cost: new Map(),
  };

  const visit = (items: ParsedItem[] | undefined) => {
    if (!items) return;
    for (const item of items) {
      if (item.supported) {
        const m = perCategory[item.category];
        m.set(item.label, (m.get(item.label) ?? 0) + 1);
      }
      visit(item.children);
    }
  };

  for (const card of cards) {
    if (!card.supported) continue;
    visit(card.parse_details);
  }

  const result: Record<HandlerTab, HandlerUsage[]> = {
    effects: [],
    triggers: [],
    keywords: [],
    statics: [],
    replacements: [],
  };

  for (const tab of Object.keys(HANDLER_TAB_TO_CATEGORIES) as HandlerTab[]) {
    const merged = new Map<string, number>();
    for (const cat of HANDLER_TAB_TO_CATEGORIES[tab]) {
      for (const [label, count] of perCategory[cat]) {
        merged.set(label, (merged.get(label) ?? 0) + count);
      }
    }
    result[tab] = [...merged.entries()]
      .map(([label, count]) => ({ label, count }))
      .sort((a, b) => b.count - a.count || a.label.localeCompare(b.label));
  }

  return result;
}

function SupportedHandlersView() {
  const [coverage, setCoverage] = useState<CoverageSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState("");
  const [activeTab, setActiveTab] = useState<HandlerTab>("effects");

  useEffect(() => {
    fetch(__COVERAGE_DATA_URL__)
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then((data: CoverageSummary) => setCoverage(data))
      .catch(() => setCoverage(null))
      .finally(() => setLoading(false));
  }, []);

  const usage = useMemo(
    () => (coverage ? extractHandlerUsage(coverage.cards) : null),
    [coverage],
  );

  const filteredItems = useMemo(() => {
    if (!usage) return [];
    const items = usage[activeTab];
    const lowerSearch = search.toLowerCase();
    if (!lowerSearch) return items;
    return items.filter((item) => item.label.toLowerCase().includes(lowerSearch));
  }, [search, activeTab, usage]);

  const tabs: { key: HandlerTab; label: string; count: number }[] = useMemo(
    () =>
      [
        { key: "effects" as const, label: "Effects" },
        { key: "triggers" as const, label: "Triggers" },
        { key: "keywords" as const, label: "Keywords" },
        { key: "statics" as const, label: "Statics" },
        { key: "replacements" as const, label: "Replacements" },
      ].map((t) => ({ ...t, count: usage?.[t.key].length ?? 0 })),
    [usage],
  );

  const totalHandlers = tabs.reduce((sum, t) => sum + t.count, 0);

  if (loading) {
    return (
      <div className="flex flex-1 items-center justify-center p-8">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-white/20 border-t-sky-300" />
      </div>
    );
  }

  if (!coverage || !usage) {
    return (
      <div className="flex-1 p-8 text-center text-sm text-slate-400">
        No coverage data available.
      </div>
    );
  }

  return (
    <>
      {/* Summary bar */}
      <div className="border-b border-white/10 px-4 py-4 sm:px-6">
        <div className="mb-2 flex flex-col gap-1 text-sm sm:flex-row sm:items-center sm:justify-between">
          <span className="text-slate-300">
            {totalHandlers} handlers observed on supported cards
          </span>
          <span className="font-mono text-xs text-slate-500 sm:text-sm">
            derived from parser output &middot; stubs &amp; unused variants excluded
          </span>
        </div>
        <div className="h-2.5 w-full overflow-hidden rounded-full bg-black/30">
          <div
            className="h-full rounded-full bg-gradient-to-r from-emerald-600 to-emerald-400"
            style={{ width: "100%" }}
          />
        </div>
      </div>

      {/* Tabs */}
      <div className="flex flex-wrap gap-2 border-b border-white/10 px-4 py-4 sm:px-6">
        {tabs.map((tab) => (
          <button
            key={tab.key}
            onClick={() => setActiveTab(tab.key)}
            className={`min-h-11 rounded-[16px] border px-4 py-2 text-sm font-semibold transition ${
              activeTab === tab.key
                ? "border-sky-400/60 bg-sky-500/14 text-sky-100"
                : "border-white/8 bg-black/20 text-slate-400 hover:border-white/14 hover:text-slate-100"
            }`}
          >
            {tab.label} ({tab.count})
          </button>
        ))}
      </div>

      {/* Search */}
      <div className="border-b border-white/10 px-4 py-4 sm:px-6">
        <input
          type="text"
          placeholder="Search..."
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="min-h-11 w-full rounded-[16px] border border-white/10 bg-black/18 px-4 py-2 text-sm text-white placeholder-slate-500 outline-none focus:border-sky-400/40"
        />
      </div>

      {/* List */}
      <div className="flex-1 overflow-y-auto p-4 sm:p-6">
        <div className="grid grid-cols-1 gap-2 sm:grid-cols-2 xl:grid-cols-3">
          {filteredItems.map((item) => (
            <div
              key={item.label}
              className="flex min-h-11 items-center gap-2 rounded-[16px] border border-white/8 bg-black/16 px-3 py-2 text-sm text-slate-200"
            >
              <span className="text-emerald-300">&#10003;</span>
              <span className="min-w-0 flex-1 truncate">{item.label}</span>
              <span className="shrink-0 font-mono text-[11px] tabular-nums text-slate-500">
                {item.count.toLocaleString()} card{item.count === 1 ? "" : "s"}
              </span>
            </div>
          ))}
        </div>
        {filteredItems.length === 0 && (
          <p className="py-8 text-center text-sm text-gray-500">
            {search
              ? `No matches found for \u201C${search}\u201D`
              : "No handlers in this category are produced by the parser on fully supported cards."}
          </p>
        )}
      </div>

      {/* Footer */}
      <div className="border-t border-white/10 px-4 py-3 text-center text-xs text-slate-500 sm:px-6">
        {totalHandlers} handlers across {tabs.length} categories &middot; engine-derived from {coverage.supported_cards.toLocaleString()} supported cards
      </div>
    </>
  );
}
