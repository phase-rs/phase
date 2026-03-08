import { useEffect, useMemo, useState } from "react";

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
  cards: CardCoverageResult[];
  missing_handler_frequency: [string, number][];
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
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/60" onClick={onClose} />
      <div className="relative z-10 flex max-h-[80vh] w-full max-w-2xl flex-col rounded-xl bg-gray-900 shadow-2xl ring-1 ring-gray-700">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-gray-800 p-4">
          <h2 className="text-lg font-bold text-white">Card Coverage Dashboard</h2>
          <button
            onClick={onClose}
            className="rounded-lg p-1 text-gray-400 transition hover:bg-gray-800 hover:text-white"
          >
            <span className="text-xl leading-none">&times;</span>
          </button>
        </div>

        {/* Main view tabs */}
        <div className="flex gap-2 border-b border-gray-800 px-4 py-2">
          <button
            onClick={() => setMainView("card-coverage")}
            className={`rounded-lg px-4 py-1.5 text-sm font-medium transition ${
              mainView === "card-coverage"
                ? "bg-indigo-600 text-white"
                : "text-gray-400 hover:bg-gray-800 hover:text-white"
            }`}
          >
            Card Coverage
          </button>
          <button
            onClick={() => setMainView("supported-handlers")}
            className={`rounded-lg px-4 py-1.5 text-sm font-medium transition ${
              mainView === "supported-handlers"
                ? "bg-indigo-600 text-white"
                : "text-gray-400 hover:bg-gray-800 hover:text-white"
            }`}
          >
            Supported Handlers
          </button>
        </div>

        {mainView === "card-coverage" ? <CardCoverageView /> : <SupportedHandlersView />}
      </div>
    </div>
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
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-gray-600 border-t-indigo-400" />
      </div>
    );
  }

  if (error || !coverage) {
    return (
      <div className="flex-1 p-6 text-center text-sm text-gray-400">
        <p className="mb-2">No coverage data available.</p>
        <p className="font-mono text-xs text-gray-500">
          Generate it with: cargo run --bin coverage-report -- /path/to/cards &gt; client/public/coverage-data.json
        </p>
      </div>
    );
  }

  if (coverage.total_cards === 0) {
    return (
      <div className="flex-1 p-6 text-center text-sm text-gray-400">
        <p className="mb-2">Coverage data is empty (0 cards analyzed).</p>
        <p className="font-mono text-xs text-gray-500">
          Run: cargo run --bin coverage-report -- /path/to/cards &gt; client/public/coverage-data.json
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

  return (
    <>
      {/* Summary bar */}
      <div className="border-b border-gray-800 px-4 py-3">
        <div className="mb-2 flex items-center justify-between text-sm">
          <span className="text-gray-300">
            {coverage.supported_cards} / {coverage.total_cards} cards supported
          </span>
          <span className="font-mono text-emerald-400">
            {coverage.coverage_pct.toFixed(1)}%
          </span>
        </div>
        <div className="h-2 w-full overflow-hidden rounded-full bg-gray-800">
          <div
            className={`h-full rounded-full bg-gradient-to-r ${progressColor}`}
            style={{ width: `${Math.min(coverage.coverage_pct, 100)}%` }}
          />
        </div>
      </div>

      {/* Filters */}
      <div className="flex gap-3 border-b border-gray-800 px-4 py-2">
        <input
          type="text"
          placeholder="Search by card name..."
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="flex-1 rounded-lg bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-indigo-500"
        />
        <select
          value={statusFilter}
          onChange={(e) => setStatusFilter(e.target.value as StatusFilter)}
          className="rounded-lg bg-gray-800 px-3 py-2 text-sm text-white outline-none ring-1 ring-gray-700 focus:ring-indigo-500"
        >
          <option value="all">All</option>
          <option value="supported">Supported</option>
          <option value="unsupported">Unsupported</option>
        </select>
      </div>

      {/* Card list */}
      <div className="flex-1 overflow-y-auto">
        <table className="w-full text-sm">
          <thead className="sticky top-0 bg-gray-900 text-left text-gray-400">
            <tr>
              <th className="px-4 py-2">Name</th>
              <th className="px-4 py-2">Status</th>
              <th className="px-4 py-2">Missing Handlers</th>
            </tr>
          </thead>
          <tbody>
            {filteredCards.map((card, i) => (
              <tr key={i} className="border-t border-gray-800/50 text-gray-300">
                <td className="px-4 py-1.5">{card.card_name}</td>
                <td className="px-4 py-1.5">
                  {card.supported ? (
                    <span className="text-emerald-400">&#10003;</span>
                  ) : (
                    <span className="text-red-400">&#10007;</span>
                  )}
                </td>
                <td className="px-4 py-1.5 text-xs text-gray-500">
                  {card.missing_handlers.join(", ")}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {filteredCards.length === 0 && (
          <p className="py-8 text-center text-sm text-gray-500">No cards match the current filters.</p>
        )}
      </div>

      {/* Missing handler frequency */}
      {coverage.missing_handler_frequency.length > 0 && (
        <div className="border-t border-gray-800 px-4 py-3">
          <h3 className="mb-2 text-xs font-semibold uppercase tracking-wide text-gray-400">
            Missing Handler Frequency
          </h3>
          <div className="max-h-32 overflow-y-auto">
            {coverage.missing_handler_frequency.map(([handler, count]) => (
              <div key={handler} className="flex items-center justify-between py-0.5 text-sm">
                <span className="text-gray-300">{handler}</span>
                <span className="rounded bg-gray-800 px-2 py-0.5 text-xs font-mono text-yellow-400">
                  {count}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Footer */}
      <div className="border-t border-gray-800 px-4 py-2 text-center text-xs text-gray-500">
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
      <div className="border-b border-gray-800 px-4 py-3">
        <div className="mb-2 flex items-center justify-between text-sm">
          <span className="text-gray-300">
            {totalHandlers} total handlers implemented
          </span>
          <span className="font-mono text-emerald-400">
            {SUPPORTED_EFFECTS.length} effects / {SUPPORTED_TRIGGERS.length} triggers / {SUPPORTED_KEYWORDS.length} keywords
          </span>
        </div>
        <div className="h-2 w-full overflow-hidden rounded-full bg-gray-800">
          <div
            className="h-full rounded-full bg-gradient-to-r from-emerald-600 to-emerald-400"
            style={{ width: "100%" }}
          />
        </div>
      </div>

      {/* Tabs */}
      <div className="flex flex-wrap gap-2 border-b border-gray-800 px-4 py-3">
        {tabs.map((tab) => (
          <button
            key={tab.key}
            onClick={() => setActiveTab(tab.key)}
            className={`rounded-lg px-3 py-1.5 text-sm font-medium transition ${
              activeTab === tab.key
                ? "bg-indigo-600 text-white"
                : "text-gray-400 hover:bg-gray-800 hover:text-white"
            }`}
          >
            {tab.label} ({tab.count})
          </button>
        ))}
      </div>

      {/* Search */}
      <div className="border-b border-gray-800 px-4 py-2">
        <input
          type="text"
          placeholder="Search..."
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="w-full rounded-lg bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-indigo-500"
        />
      </div>

      {/* List */}
      <div className="flex-1 overflow-y-auto p-4">
        <div className="grid grid-cols-2 gap-1 sm:grid-cols-3">
          {filteredItems.map((item) => (
            <div
              key={item}
              className="flex items-center gap-2 rounded px-2 py-1 text-sm text-gray-300"
            >
              <span className="text-emerald-400">&#10003;</span>
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
      <div className="border-t border-gray-800 px-4 py-3 text-center text-xs text-gray-500">
        {totalHandlers} total handlers across {tabs.length} categories
      </div>
    </>
  );
}
