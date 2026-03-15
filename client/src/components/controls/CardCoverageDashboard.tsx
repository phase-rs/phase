import { useEffect, useMemo, useState } from "react";
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

interface CardCoverageResult {
  card_name: string;
  set_code: string;
  supported: boolean;
  missing_handlers: string[];
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
      maxWidthClassName="max-w-5xl"
      bodyClassName="flex flex-col overflow-hidden"
    >
        <div className="flex flex-wrap gap-2 border-b border-white/10 px-4 py-4 sm:px-6">
          <button
            onClick={() => setMainView("card-coverage")}
            className={`min-h-11 rounded-[16px] border px-4 py-2 text-sm font-semibold transition ${
              mainView === "card-coverage"
                ? "border-sky-400/60 bg-sky-500/14 text-sky-100"
                : "border-white/8 bg-black/20 text-slate-400 hover:border-white/14 hover:text-slate-100"
            }`}
          >
            Card Coverage
          </button>
          <button
            onClick={() => setMainView("supported-handlers")}
            className={`min-h-11 rounded-[16px] border px-4 py-2 text-sm font-semibold transition ${
              mainView === "supported-handlers"
                ? "border-sky-400/60 bg-sky-500/14 text-sky-100"
                : "border-white/8 bg-black/20 text-slate-400 hover:border-white/14 hover:text-slate-100"
            }`}
          >
            Supported Handlers
          </button>
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

  const filteredCards = useMemo(() => {
    if (!coverage) return [];
    const lowerSearch = search.toLowerCase();
    return coverage.cards.filter((card) => {
      if (statusFilter === "supported" && !card.supported) return false;
      if (statusFilter === "unsupported" && card.supported) return false;
      if (lowerSearch && !card.card_name.toLowerCase().includes(lowerSearch)) return false;
      return true;
    });
  }, [coverage, search, statusFilter]);

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

  return (
    <>
      {/* Summary bar */}
      <div className="border-b border-white/10 px-4 py-4 sm:px-6">
        <div className="mb-2 flex items-center justify-between text-sm">
          <span className="text-slate-300">
            {coverage.supported_cards} / {coverage.total_cards} cards supported
          </span>
          <span className="font-mono text-emerald-300">
            {coverage.coverage_pct.toFixed(1)}%
          </span>
        </div>
        <div className="h-2.5 w-full overflow-hidden rounded-full bg-black/30">
          <div
            className={`h-full rounded-full bg-gradient-to-r ${progressColor}`}
            style={{ width: `${Math.min(coverage.coverage_pct, 100)}%` }}
          />
        </div>
        {formatCoverage.length > 0 && (
          <div className="mt-4 grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
            {formatCoverage.map(([format, summary]) => (
              <div
                key={format}
                className="rounded-[16px] border border-white/8 bg-black/18 px-3 py-3"
              >
                <div className="text-[11px] font-semibold uppercase tracking-[0.16em] text-slate-500">
                  {format}
                </div>
                <div className="mt-1 text-sm text-slate-200">
                  {summary.supported_cards} / {summary.total_cards} fully supported
                </div>
                <div className="mt-1 font-mono text-xs text-emerald-300">
                  {summary.coverage_pct.toFixed(1)}%
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Filters */}
      <div className="flex flex-col gap-3 border-b border-white/10 px-4 py-4 sm:flex-row sm:px-6">
        <input
          type="text"
          placeholder="Search by card name..."
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="min-h-11 flex-1 rounded-[16px] border border-white/10 bg-black/18 px-4 py-2 text-sm text-white placeholder-slate-500 outline-none focus:border-sky-400/40"
        />
        <select
          value={statusFilter}
          onChange={(e) => setStatusFilter(e.target.value as StatusFilter)}
          className="min-h-11 rounded-[16px] border border-white/10 bg-black/18 px-4 py-2 text-sm text-white outline-none focus:border-sky-400/40"
        >
          <option value="all">All</option>
          <option value="supported">Supported</option>
          <option value="unsupported">Unsupported</option>
        </select>
      </div>

      {/* Card list */}
      <div className="hidden flex-1 overflow-y-auto md:block">
        <table className="w-full text-sm">
          <thead className="sticky top-0 bg-[#0b1020]/98 text-left text-slate-500 backdrop-blur-md">
            <tr>
              <th className="px-6 py-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em]">Name</th>
              <th className="px-6 py-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em]">Status</th>
              <th className="px-6 py-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em]">Missing Handlers</th>
            </tr>
          </thead>
          <tbody>
            {filteredCards.map((card, i) => (
              <tr key={i} className="border-t border-white/6 text-slate-200 transition hover:bg-white/[0.03]">
                <td className="px-6 py-3">{card.card_name}</td>
                <td className="px-6 py-3">
                  {card.supported ? (
                    <span className="rounded-full border border-emerald-400/30 bg-emerald-500/12 px-2 py-1 text-xs font-semibold uppercase tracking-[0.16em] text-emerald-300">
                      Supported
                    </span>
                  ) : (
                    <span className="rounded-full border border-rose-400/30 bg-rose-500/12 px-2 py-1 text-xs font-semibold uppercase tracking-[0.16em] text-rose-300">
                      Missing
                    </span>
                  )}
                </td>
                <td className="px-6 py-3 text-xs text-slate-500">
                  {card.missing_handlers.length > 0 ? card.missing_handlers.join(", ") : "Fully covered"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {filteredCards.length === 0 && (
          <p className="py-10 text-center text-sm text-slate-500">No cards match the current filters.</p>
        )}
      </div>
      <div className="flex-1 overflow-y-auto px-4 py-3 md:hidden">
        <div className="space-y-3">
          {filteredCards.map((card, i) => (
            <article
              key={`${card.card_name}-${i}`}
              className="rounded-[18px] border border-white/8 bg-black/16 p-4"
            >
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="text-sm font-semibold text-slate-100">{card.card_name}</div>
                  <div className="mt-1 text-[11px] uppercase tracking-[0.16em] text-slate-500">
                    {card.set_code || "Unknown Set"}
                  </div>
                </div>
                {card.supported ? (
                  <span className="shrink-0 rounded-full border border-emerald-400/30 bg-emerald-500/12 px-2 py-1 text-[11px] font-semibold uppercase tracking-[0.16em] text-emerald-300">
                    Supported
                  </span>
                ) : (
                  <span className="shrink-0 rounded-full border border-rose-400/30 bg-rose-500/12 px-2 py-1 text-[11px] font-semibold uppercase tracking-[0.16em] text-rose-300">
                    Missing
                  </span>
                )}
              </div>
              <div className="mt-3">
                <div className="text-[11px] font-semibold uppercase tracking-[0.16em] text-slate-500">
                  Missing Handlers
                </div>
                <div className="mt-1 text-xs text-slate-400">
                  {card.missing_handlers.length > 0 ? card.missing_handlers.join(", ") : "Fully covered"}
                </div>
              </div>
            </article>
          ))}
        </div>
        {filteredCards.length === 0 && (
          <p className="py-10 text-center text-sm text-slate-500">No cards match the current filters.</p>
        )}
      </div>

      {/* Missing handler frequency */}
      {coverage.missing_handler_frequency.length > 0 && (
        <div className="border-t border-white/10 px-4 py-4 sm:px-6">
          <h3 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
            Missing Handler Frequency
          </h3>
          <div className="max-h-36 space-y-1 overflow-y-auto">
            {coverage.missing_handler_frequency.map(([handler, count]) => (
              <div key={handler} className="flex items-center justify-between rounded-[14px] border border-white/6 bg-black/16 px-3 py-2 text-sm">
                <span className="text-slate-200">{handler}</span>
                <span className="rounded-full border border-amber-400/20 bg-amber-500/10 px-2.5 py-1 text-xs font-mono text-amber-300">
                  {count}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Footer */}
      <div className="border-t border-white/10 px-4 py-3 text-center text-xs text-slate-500 sm:px-6">
        Showing {filteredCards.length} of {coverage.total_cards} cards
      </div>
    </>
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
