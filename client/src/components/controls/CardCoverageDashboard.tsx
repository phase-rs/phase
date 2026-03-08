import { useMemo, useState } from "react";

/** Known effect API types supported by the engine. */
const SUPPORTED_EFFECTS = [
  "DealDamage", "GainLife", "LoseLife", "DrawCard", "Discard",
  "DestroyTarget", "DestroyAll", "ExileTarget", "BounceTarget",
  "CounterSpell", "PutCounter", "RemoveCounter", "CreateToken",
  "PumpTarget", "PumpAll", "TapTarget", "UntapTarget",
  "GainControl", "Sacrifice", "Mill", "Scry", "Surveil",
  "FightTarget", "AttachTarget", "Search", "Regenerate",
  "PreventDamage", "SetPowerToughness", "SwitchPowerToughness",
  "CopySpell", "Proliferate", "Transform", "Manifest",
  "PhaseOut", "Populate", "Amass", "Adapt", "Explore",
] as const;

/** Known trigger modes supported by the engine. */
const SUPPORTED_TRIGGERS = [
  "ChangesZone", "Attacks", "Blocks", "BecomesTarget",
  "SpellCast", "AbilityActivated", "Damaged", "Untaps",
  "TurnBegin", "PhaseBegin", "Draws", "Discards",
  "LandPlayed", "LifeGained", "LifeLost", "CounterAdded",
  "CounterRemoved", "TokenCreated", "Dies", "Sacrificed",
  "Destroyed", "Exiled", "Returned", "Attached",
  "Detached", "Transformed", "TappedForMana",
] as const;

/** Known keyword abilities. */
const SUPPORTED_KEYWORDS = [
  "Flying", "First Strike", "Double Strike", "Deathtouch",
  "Haste", "Hexproof", "Indestructible", "Lifelink",
  "Menace", "Reach", "Trample", "Vigilance",
  "Flash", "Defender", "Fear", "Intimidate",
  "Shroud", "Protection", "Ward", "Prowess",
  "Convoke", "Delve", "Cascade", "Cycling",
  "Equip", "Enchant", "Kicker", "Flashback",
  "Retrace", "Unearth", "Bestow", "Morph",
  "Emerge", "Evoke", "Suspend", "Madness",
  "Miracle", "Overload", "Entwine", "Buyback",
  "Affinity", "Metalcraft", "Landfall", "Revolt",
  "Ferocious", "Heroic", "Constellation", "Devotion",
  "Crew", "Fabricate", "Exploit", "Outlast",
] as const;

interface CardCoverageDashboardProps {
  onClose: () => void;
}

export function CardCoverageDashboard({ onClose }: CardCoverageDashboardProps) {
  const [search, setSearch] = useState("");
  const [activeTab, setActiveTab] = useState<"effects" | "triggers" | "keywords">("effects");

  const filteredItems = useMemo(() => {
    const lowerSearch = search.toLowerCase();
    const items =
      activeTab === "effects"
        ? SUPPORTED_EFFECTS
        : activeTab === "triggers"
          ? SUPPORTED_TRIGGERS
          : SUPPORTED_KEYWORDS;

    if (!lowerSearch) return items;
    return items.filter((item) => item.toLowerCase().includes(lowerSearch));
  }, [search, activeTab]);

  const tabs = [
    { key: "effects" as const, label: "Effects", count: SUPPORTED_EFFECTS.length },
    { key: "triggers" as const, label: "Triggers", count: SUPPORTED_TRIGGERS.length },
    { key: "keywords" as const, label: "Keywords", count: SUPPORTED_KEYWORDS.length },
  ];

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

        {/* Summary stats */}
        <div className="flex gap-4 border-b border-gray-800 px-4 py-3">
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
          {SUPPORTED_EFFECTS.length} effect types | {SUPPORTED_TRIGGERS.length} trigger modes | {SUPPORTED_KEYWORDS.length} keywords
        </div>
      </div>
    </div>
  );
}
