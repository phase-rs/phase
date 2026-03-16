import { useCallback, useEffect, useMemo, useState } from "react";
import { ModalPanelShell } from "../ui/ModalPanelShell";

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
  children?: ParsedItem[];
}

interface CardCoverageResult {
  card_name: string;
  set_code: string;
  supported: boolean;
  missing_handlers: string[];
  oracle_text?: string;
  parse_details?: ParsedItem[];
}

interface CoverageSummary {
  total_cards: number;
  supported_cards: number;
  coverage_pct: number;
  coverage_by_format?: Record<string, FormatCoverageSummary>;
  cards: CardCoverageResult[];
  missing_handler_frequency: [string, number][];
}

interface FormatCoverageSummary {
  total_cards: number;
  supported_cards: number;
  coverage_pct: number;
}

const MAX_VISIBLE_CARDS = 200;

type MainView = "card-coverage" | "supported-handlers";
type HandlerTab = "effects" | "triggers" | "keywords" | "statics" | "replacements";
type StatusFilter = "all" | "supported" | "unsupported";

interface CardCoverageDashboardProps {
  onClose: () => void;
}

export function CardCoverageDashboard({ onClose }: CardCoverageDashboardProps) {
  const [mainView, setMainView] = useState<MainView>("card-coverage");

  return (
    <ModalPanelShell
      title="Card Coverage"
      subtitle="Inspect implementation coverage and supported engine handlers."
      onClose={onClose}
      maxWidthClassName="max-w-7xl"
      bodyClassName="flex flex-col overflow-hidden"
    >
        <div className="flex flex-wrap gap-2 border-b border-white/10 px-4 py-3 sm:px-6">
          {(["card-coverage", "supported-handlers"] as const).map((view) => (
            <button
              key={view}
              onClick={() => setMainView(view)}
              className={`rounded-[16px] border px-4 py-1.5 text-sm font-semibold transition ${
                mainView === view
                  ? "border-sky-400/60 bg-sky-500/14 text-sky-100"
                  : "border-white/8 bg-black/20 text-slate-400 hover:border-white/14 hover:text-slate-100"
              }`}
            >
              {view === "card-coverage" ? "Card Coverage" : "Supported Handlers"}
            </button>
          ))}
        </div>

        {mainView === "card-coverage" ? <CardCoverageView /> : <SupportedHandlersView />}
    </ModalPanelShell>
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

  useEffect(() => {
    fetch("/coverage-data.json")
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
    return coverage.cards.filter((card) => {
      if (statusFilter === "supported" && !card.supported) return false;
      if (statusFilter === "unsupported" && card.supported) return false;
      if (lowerSearch && !card.card_name.toLowerCase().includes(lowerSearch)) return false;
      return true;
    });
  }, [coverage, search, statusFilter, hasActiveFilter]);

  const activeCard = useMemo(() => {
    if (!selectedCard) return null;
    return filteredCards.find((_, i) => `${filteredCards[i].card_name}-${i}` === selectedCard) ?? null;
  }, [selectedCard, filteredCards]);

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
                <span className="mx-1.5 text-white/10">·</span>
              </span>
            ))}
          </div>
        )}
      </div>

      {/* Master-detail split */}
      <div className="flex min-h-0 flex-1">
        {/* Left panel: card list */}
        <div className="flex w-80 shrink-0 flex-col border-r border-white/10">
          {/* Search & filter */}
          <div className="flex gap-2 px-3 py-3">
            <input
              type="text"
              placeholder="Search cards..."
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              className="min-w-0 flex-1 rounded-[12px] border border-white/10 bg-black/18 px-3 py-1.5 text-sm text-white placeholder-slate-500 outline-none focus:border-sky-400/40"
            />
            <select
              value={statusFilter}
              onChange={(e) => setStatusFilter(e.target.value as StatusFilter)}
              className="rounded-[12px] border border-white/10 bg-black/18 px-2 py-1.5 text-xs text-white outline-none focus:border-sky-400/40"
            >
              <option value="all">All</option>
              <option value="supported">Supported</option>
              <option value="unsupported">Unsupported</option>
            </select>
          </div>

          {/* Scrollable card list */}
          <div className="min-h-0 flex-1 overflow-y-auto">
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
                  return (
                    <button
                      key={cardKey}
                      onClick={() => setSelectedCard(isSelected ? null : cardKey)}
                      className={`flex w-full items-center gap-2 px-3 py-2 text-left text-[13px] transition ${
                        isSelected
                          ? "bg-sky-500/10 text-white"
                          : "text-slate-300 hover:bg-white/[0.03]"
                      }`}
                    >
                      <span
                        className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                          card.supported ? "bg-emerald-400" : "bg-rose-400"
                        }`}
                      />
                      <span className="min-w-0 flex-1 truncate">{card.card_name}</span>
                      {!card.supported && card.missing_handlers.length > 0 && (
                        <span className="shrink-0 text-[10px] tabular-nums text-rose-400/70">
                          {card.missing_handlers.length}
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
          Hover over oracle text or tree nodes to see how they connect
        </div>
      </div>

      {/* Format coverage grid as detail content */}
      {formatCoverage.length > 0 && (
        <div className="mt-4 w-full max-w-lg">
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

      {/* Missing handler frequency */}
      {coverage.missing_handler_frequency.length > 0 && (
        <div className="mt-6 w-full max-w-lg">
          <div className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
            Most Common Missing Handlers
          </div>
          <div className="space-y-1">
            {coverage.missing_handler_frequency.slice(0, 10).map(([handler, count]) => (
              <div
                key={handler}
                className="flex items-center justify-between rounded-[10px] border border-white/5 bg-black/12 px-3 py-1.5 text-[13px]"
              >
                <span className="text-slate-300">{handler}</span>
                <span className="font-mono text-xs text-amber-300/80">{count}</span>
              </div>
            ))}
          </div>
        </div>
      )}
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
      <div className="mb-4 flex items-baseline gap-3">
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
                      className={`cursor-default rounded-[3px] border-b transition-colors duration-100 ${
                        hoveredId === seg.itemId
                          ? `${CATEGORY_BG_COLORS[seg.item!.category]} ${CATEGORY_UNDERLINE_COLORS[seg.item!.category]}`
                          : `border-transparent ${seg.item!.supported ? "text-slate-200" : "text-rose-300"}`
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

      {/* Missing handlers summary */}
      {card.missing_handlers.length > 0 && (
        <div className="mt-3 text-xs text-slate-500">
          <span className="font-semibold text-rose-400">Missing: </span>
          {card.missing_handlers.join(", ")}
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
