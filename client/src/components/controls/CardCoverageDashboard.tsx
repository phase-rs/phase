import { useCallback, useEffect, useMemo, useRef, useState } from "react";

/** Known effect API types supported by the engine. */
const SUPPORTED_EFFECTS = [
  "DealDamage", "GainLife", "LoseLife", "Draw", "DiscardCard",
  "Destroy", "ChangeZone", "Counter",
  "AddCounter", "RemoveCounter", "Token",
  "Pump", "Tap", "Untap",
  "Sacrifice",
] as const;

/** Known trigger modes supported by the engine (with real handlers). */
const SUPPORTED_TRIGGERS = [
  "ChangesZone", "ChangesZoneAll",
  "DamageDone", "DamageDoneOnce", "DamageAll", "DamageDealtOnce", "DamageDoneOnceByController",
  "SpellCast", "SpellCastOrCopy",
  "Attacks", "AttackersDeclared", "AttackersDeclaredOneTarget",
  "Blocks", "BlockersDeclared",
  "Countered",
  "CounterAdded", "CounterAddedOnce", "CounterAddedAll",
  "CounterRemoved", "CounterRemovedOnce",
  "Taps", "TapAll", "Untaps", "UntapAll",
  "LifeGained", "LifeLost", "LifeLostAll",
] as const;

/** Known keyword abilities (non-Unknown variants). */
const SUPPORTED_KEYWORDS = [
  "Flying", "First Strike", "Double Strike", "Deathtouch",
  "Haste", "Hexproof", "Indestructible", "Lifelink",
  "Menace", "Reach", "Trample", "Vigilance",
  "Flash", "Defender", "Fear", "Intimidate",
  "Shroud", "Skulk", "Shadow", "Horsemanship",
  "Wither", "Infect", "Afflict",
  "Prowess", "Undying", "Persist", "Cascade",
  "Exalted", "Flanking", "Evolve", "Extort",
  "Exploit", "Explore", "Ascend", "Soulbond",
  "Convoke", "Delve", "Devoid", "Changeling", "Phasing",
  "Protection", "Ward",
  "Cycling", "Equip", "Enchant", "Kicker", "Flashback",
  "Retrace", "Bestow", "Morph", "Emerge", "Evoke",
  "Suspend", "Madness", "Miracle", "Overload",
  "Entwine", "Buyback", "Affinity",
  "Crew", "Fabricate", "Outlast",
] as const;

/** Static ability modes supported by the engine. */
const SUPPORTED_STATICS = [
  "Continuous",
  "CantAttack", "CantBlock", "CantBeTargeted", "CantBeCast",
  "CantBeActivated", "CastWithFlash", "ReduceCost", "RaiseCost",
  "CantGainLife", "CantLoseLife", "MustAttack", "MustBlock",
  "CantDraw", "Panharmonicon", "IgnoreHexproof",
] as const;

/** Replacement effect types with real handlers. */
const SUPPORTED_REPLACEMENTS = [
  "DamageDone", "Moved", "Destroy", "Draw",
  "GainLife", "LifeReduced",
  "AddCounter", "RemoveCounter",
  "Tap", "Untap", "Counter", "CreateToken",
] as const;

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

type MainView = "card-coverage" | "gap-analysis" | "supported-handlers";
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
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-[20px] border border-white/10 bg-[#0b1020]/96 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md sm:rounded-[24px]">
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
      <div className="flex flex-wrap gap-2 border-b border-white/10 px-4 py-3 sm:px-6">
        {(["card-coverage", "gap-analysis", "supported-handlers"] as const).map((view) => (
          <button
            key={view}
            onClick={() => setMainView(view)}
            className={`rounded-[16px] border px-4 py-1.5 text-sm font-semibold transition ${
              mainView === view
                ? "border-sky-400/60 bg-sky-500/14 text-sky-100"
                : "border-white/8 bg-black/20 text-slate-400 hover:border-white/14 hover:text-slate-100"
            }`}
          >
            {view === "card-coverage" ? "Card Coverage" : view === "gap-analysis" ? "Gap Analysis" : "Supported Handlers"}
          </button>
        ))}
      </div>

      {/* Content */}
      {mainView === "card-coverage" ? (
        <CardCoverageView />
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
    if (!coverage || !hasActiveFilter) return [];
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
  }, [coverage, search, statusFilter, hasActiveFilter, sortMode]);

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
  const formatCoverage = Object.entries(coverage.coverage_by_format ?? {}).filter(
    ([, summary]) => summary.total_cards > 0,
  );

  const visibleCards = filteredCards.slice(0, MAX_VISIBLE_CARDS);

  return (
    <>
      {/* Condensed summary bar */}
      <div className="flex items-center gap-4 border-b border-white/10 px-4 py-3 sm:px-6">
        <div className="flex shrink-0 items-center gap-3 text-sm">
          <span className="text-slate-300">
            {coverage.supported_cards.toLocaleString()} / {coverage.total_cards.toLocaleString()}
          </span>
          <div className="h-2 w-24 overflow-hidden rounded-full bg-black/30">
            <div
              className={`h-full rounded-full bg-gradient-to-r ${progressColor}`}
              style={{ width: `${Math.min(coverage.coverage_pct, 100)}%` }}
            />
          </div>
          <span className="font-mono text-sm text-emerald-300">
            {coverage.coverage_pct.toFixed(1)}%
          </span>
        </div>
        {formatCoverage.length > 0 && (
          <div className="hidden min-w-0 truncate text-xs text-slate-500 lg:block">
            {formatCoverage.map(([format, summary]) => (
              <span key={format}>
                <span className="uppercase">{format.slice(0, 3)}</span>
                {" "}
                <span className="text-slate-400">{summary.coverage_pct.toFixed(0)}%</span>
                <span className="mx-1.5 text-white/10">&middot;</span>
              </span>
            ))}
          </div>
        )}
      </div>

      {/* Master-detail split */}
      <div className="flex min-h-0 flex-1" onKeyDown={handleKeyDown}>
        {/* Left panel: card list */}
        <div className="flex w-80 shrink-0 flex-col border-r border-white/10">
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
            {!hasActiveFilter ? (
              <div className="px-3 py-10 text-center">
                <div className="text-xs text-slate-500">
                  Search or filter to browse {coverage.total_cards.toLocaleString()} cards
                </div>
              </div>
            ) : (
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
            )}
          </div>

          {/* List footer */}
          <div className="border-t border-white/8 px-3 py-2 text-center text-[11px] text-slate-600">
            {hasActiveFilter
              ? `${Math.min(filteredCards.length, MAX_VISIBLE_CARDS)} of ${filteredCards.length.toLocaleString()} matches`
              : `${coverage.total_cards.toLocaleString()} cards`}
          </div>
        </div>

        {/* Right panel: card detail */}
        <div className="min-h-0 min-w-0 flex-1 overflow-y-auto">
          {activeCard ? (
            <CardParseDetail card={activeCard} />
          ) : (
            <DetailEmptyState coverage={coverage} />
          )}
        </div>
      </div>
    </>
  );
}

/** Summary view shown in the detail panel when no card is selected. */
function DetailEmptyState({ coverage }: { coverage: CoverageSummary }) {
  const formatCoverage = Object.entries(coverage.coverage_by_format ?? {}).filter(
    ([, summary]) => summary.total_cards > 0,
  );

  return (
    <div className="flex h-full flex-col items-center justify-center px-8 py-12">
      <div className="mb-8 text-center">
        <div className="text-sm text-slate-400">Select a card to inspect its parse breakdown</div>
        <div className="mt-1 text-xs text-slate-600">
          Use arrow keys to navigate, Escape to deselect
        </div>
      </div>

      {/* Format coverage grid */}
      {formatCoverage.length > 0 && (
        <div className="w-full max-w-lg">
          <div className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
            Coverage by Format
          </div>
          <div className="grid grid-cols-2 gap-2">
            {formatCoverage.map(([format, summary]) => (
              <div
                key={format}
                className="rounded-[12px] border border-white/6 bg-black/16 px-3 py-2.5"
              >
                <div className="flex items-baseline justify-between">
                  <span className="text-[11px] font-semibold uppercase tracking-[0.14em] text-slate-500">
                    {format}
                  </span>
                  <span className="font-mono text-xs text-emerald-300">
                    {summary.coverage_pct.toFixed(1)}%
                  </span>
                </div>
                <div className="mt-1.5 h-1.5 w-full overflow-hidden rounded-full bg-black/30">
                  <div
                    className="h-full rounded-full bg-gradient-to-r from-emerald-600 to-emerald-400"
                    style={{ width: `${Math.min(summary.coverage_pct, 100)}%` }}
                  />
                </div>
                <div className="mt-1 text-[11px] text-slate-500">
                  {summary.supported_cards.toLocaleString()} / {summary.total_cards.toLocaleString()}
                </div>
              </div>
            ))}
          </div>
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
        <span className="shrink-0 font-mono text-xs text-amber-300/80">{gap.total_count} total</span>
        {gap.single_gap_cards > 0 && (
          <span className="shrink-0 font-mono text-xs text-emerald-300/80">
            {gap.single_gap_cards} unlock
          </span>
        )}
        {ratioStr && (
          <span className="shrink-0 rounded-full bg-sky-500/12 px-2 py-0.5 text-[10px] text-sky-300">
            {ratioStr} ind
          </span>
        )}
      </button>

      {isExpanded && (
        <div className="ml-6 mt-1 space-y-2 border-l border-white/8 pl-4 pt-1">
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

function CardParseDetail({ card }: { card: CardCoverageResult }) {
  const [hoveredId, setHoveredId] = useState<string | null>(null);

  const onHover = useCallback((id: string | null) => setHoveredId(id), []);

  const indexed = useMemo(
    () => flattenParseItems(card.parse_details ?? []),
    [card.parse_details],
  );

  const annotatedLines = useMemo(() => {
    if (!card.oracle_text) return [];
    return card.oracle_text.split("\n").map((line) => ({
      line,
      segments: annotateOracleLine(line, indexed),
    }));
  }, [card.oracle_text, indexed]);

  const grouped = useMemo(
    () =>
      (card.parse_details ?? []).reduce<Record<string, ParsedItem[]>>((groups, item) => {
        (groups[item.category] ??= []).push(item);
        return groups;
      }, {}),
    [card.parse_details],
  );

  const categoryOrder: ParseCategory[] = ["keyword", "ability", "trigger", "static", "replacement", "cost"];
  const activeCategories = categoryOrder.filter((c) => grouped[c]?.length);

  return (
    <div className="px-5 py-4 sm:px-6">
      {/* Card name header */}
      <div className="mb-4 flex items-center gap-3">
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
          <CopyButton text={card.oracle_text} label="Copy Oracle" className="ml-auto" />
        )}
      </div>

      {/* Annotated oracle text */}
      {annotatedLines.length > 0 && (
        <div className="mb-5">
          <div className="mb-1.5 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
            Oracle Text
          </div>
          <div className="rounded-[12px] border border-white/8 bg-black/24 px-3 py-2.5 font-mono text-xs leading-[1.8] text-slate-300">
            {annotatedLines.map(({ segments }, lineIdx) => (
              <div key={lineIdx}>
                {segments.map((seg, segIdx) =>
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
            ))}
          </div>
        </div>
      )}

      {/* Parse tree */}
      {activeCategories.length > 0 ? (
        <div className="space-y-3">
          <div className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
            Parse breakdown
          </div>
          {activeCategories.map((category) => (
            <div key={category}>
              <div className={`mb-1 text-[11px] font-semibold uppercase tracking-[0.16em] ${CATEGORY_COLORS[category]}`}>
                {CATEGORY_LABELS[category]}s ({grouped[category].length})
              </div>
              <div className="space-y-0.5">
                {grouped[category].map((item, i) => (
                  <ParseTreeNode
                    key={i}
                    item={item}
                    depth={0}
                    idPrefix={`${category}-${i}`}
                    hoveredId={hoveredId}
                    onHover={onHover}
                    indexed={indexed}
                  />
                ))}
              </div>
            </div>
          ))}
        </div>
      ) : (
        <div className="text-xs text-slate-500">No parsed items (vanilla card).</div>
      )}

      {/* Gap details — vertical list with source text */}
      {(card.gap_details?.length ?? 0) > 0 && (
        <div className="mt-4">
          <div className="mb-2 flex items-center gap-2">
            <span className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-rose-400">
              Missing Handlers ({card.gap_count ?? 0})
            </span>
            <CopyButton
              text={card.gap_details!.map((g) => g.source_text ? `${g.handler}: ${g.source_text}` : g.handler).join("\n")}
              label="Copy"
            />
          </div>
          <div className="space-y-1">
            {card.gap_details!.map((gap) => (
              <div
                key={gap.handler}
                className="rounded-[8px] border border-rose-400/10 bg-rose-500/5 px-3 py-1.5"
              >
                <div className="text-xs font-medium text-rose-300">{gap.handler}</div>
                {gap.source_text && (
                  <div className="mt-0.5 font-mono text-[11px] text-rose-300/50">
                    {gap.source_text}
                  </div>
                )}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function ParseTreeNode({
  item,
  depth,
  idPrefix,
  hoveredId,
  onHover,
  indexed,
}: {
  item: ParsedItem;
  depth: number;
  idPrefix: string;
  hoveredId: string | null;
  onHover: (id: string | null) => void;
  indexed: IndexedItem[];
}) {
  const hasChildren = (item.children?.length ?? 0) > 0;
  const dotColor = item.supported ? "text-emerald-400" : "text-rose-400";
  const labelColor = item.supported ? "text-slate-200" : "text-rose-300";

  // Find the matching indexed ID for this item instance
  const matchedEntry = indexed.find((e) => e.item === item);
  const itemId = matchedEntry?.id ?? null;
  const isHighlighted = itemId !== null && hoveredId === itemId;

  return (
    <div style={{ paddingLeft: `${depth * 16}px` }}>
      <div
        className={`flex items-start gap-1.5 rounded-[8px] px-2 py-1 transition-colors duration-100 ${
          isHighlighted
            ? `${CATEGORY_BG_COLORS[item.category]} ring-1 ring-inset ${CATEGORY_RING_COLORS[item.category]}`
            : "hover:bg-white/[0.03]"
        }`}
        onMouseEnter={() => itemId && onHover(itemId)}
        onMouseLeave={() => onHover(null)}
      >
        <span className={`mt-0.5 text-[8px] ${dotColor}`}>&#9679;</span>
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span className={`text-xs font-medium ${labelColor}`}>{item.label}</span>
            <span className={`text-[10px] ${CATEGORY_COLORS[item.category]}`}>
              {CATEGORY_LABELS[item.category]}
            </span>
            {!item.supported && (
              <span className="text-[10px] font-semibold uppercase tracking-[0.12em] text-rose-400">
                unsupported
              </span>
            )}
          </div>
          {item.source_text && (
            <div className="mt-0.5 font-mono text-[11px] leading-snug text-slate-500">
              &ldquo;{item.source_text}&rdquo;
            </div>
          )}
          {item.details && item.details.length > 0 && (
            <div className="mt-1 flex flex-wrap gap-1">
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
      {hasChildren &&
        item.children!.map((child, i) => (
          <ParseTreeNode
            key={i}
            item={child}
            depth={depth + 1}
            idPrefix={`${idPrefix}-${i}`}
            hoveredId={hoveredId}
            onHover={onHover}
            indexed={indexed}
          />
        ))}
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

// --- Supported Handlers View (existing functionality) ---

function SupportedHandlersView() {
  const [search, setSearch] = useState("");
  const [activeTab, setActiveTab] = useState<HandlerTab>("effects");

  const filteredItems = useMemo(() => {
    const lowerSearch = search.toLowerCase();
    const items =
      activeTab === "effects"
        ? SUPPORTED_EFFECTS
        : activeTab === "triggers"
          ? SUPPORTED_TRIGGERS
          : activeTab === "keywords"
            ? SUPPORTED_KEYWORDS
            : activeTab === "statics"
              ? SUPPORTED_STATICS
              : SUPPORTED_REPLACEMENTS;

    if (!lowerSearch) return items;
    return items.filter((item) => item.toLowerCase().includes(lowerSearch));
  }, [search, activeTab]);

  const tabs: { key: HandlerTab; label: string; count: number }[] = [
    { key: "effects", label: "Effects", count: SUPPORTED_EFFECTS.length },
    { key: "triggers", label: "Triggers", count: SUPPORTED_TRIGGERS.length },
    { key: "keywords", label: "Keywords", count: SUPPORTED_KEYWORDS.length },
    { key: "statics", label: "Statics", count: SUPPORTED_STATICS.length },
    { key: "replacements", label: "Replacements", count: SUPPORTED_REPLACEMENTS.length },
  ];

  const totalHandlers =
    SUPPORTED_EFFECTS.length +
    SUPPORTED_TRIGGERS.length +
    SUPPORTED_KEYWORDS.length +
    SUPPORTED_STATICS.length +
    SUPPORTED_REPLACEMENTS.length;

  return (
    <>
      {/* Summary bar */}
      <div className="border-b border-white/10 px-4 py-4 sm:px-6">
        <div className="mb-2 flex items-center justify-between text-sm">
          <span className="text-slate-300">
            {totalHandlers} total handlers implemented
          </span>
          <span className="font-mono text-emerald-300">
            {SUPPORTED_EFFECTS.length} effects / {SUPPORTED_TRIGGERS.length} triggers / {SUPPORTED_KEYWORDS.length} keywords
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
              key={item}
              className="flex min-h-11 items-center gap-2 rounded-[16px] border border-white/8 bg-black/16 px-3 py-2 text-sm text-slate-200"
            >
              <span className="text-emerald-300">&#10003;</span>
              {item}
            </div>
          ))}
        </div>
        {filteredItems.length === 0 && (
          <p className="py-8 text-center text-sm text-gray-500">
            No matches found for &ldquo;{search}&rdquo;
          </p>
        )}
      </div>

      {/* Footer */}
      <div className="border-t border-white/10 px-4 py-3 text-center text-xs text-slate-500 sm:px-6">
        {totalHandlers} total handlers across {tabs.length} categories
      </div>
    </>
  );
}
