import { useMemo, useState } from "react";

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

type TabKey = "effects" | "triggers" | "keywords" | "statics" | "replacements";

interface CardCoverageDashboardProps {
  onClose: () => void;
}

export function CardCoverageDashboard({ onClose }: CardCoverageDashboardProps) {
  const [search, setSearch] = useState("");
  const [activeTab, setActiveTab] = useState<TabKey>("effects");

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

  const tabs: { key: TabKey; label: string; count: number }[] = [
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
      </div>
    </div>
  );
}
