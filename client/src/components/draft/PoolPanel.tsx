import { useMemo } from "react";

import { useDraftStore } from "../../stores/draftStore";
import type { PoolSortMode } from "../../stores/draftStore";
import type { DraftCardInstance } from "../../adapter/draft-adapter";

// ── Sorting helpers ─────────────────────────────────────────────────────
// These are display-layer grouping of engine-provided enriched fields
// (DraftCardInstance.colors, cmc, type_line) — not game logic computation.

function colorGroupKey(card: DraftCardInstance): string {
  if (card.colors.length === 0) return "Colorless";
  if (card.colors.length > 1) return "Multicolor";
  return card.colors[0];
}

const COLOR_GROUP_ORDER: Record<string, number> = {
  W: 0, U: 1, B: 2, R: 3, G: 4, Multicolor: 5, Colorless: 6,
};

const COLOR_GROUP_LABELS: Record<string, string> = {
  W: "White", U: "Blue", B: "Black", R: "Red", G: "Green",
  Multicolor: "Multicolor", Colorless: "Colorless",
};

function primaryType(typeLine: string): string {
  const lower = typeLine.toLowerCase();
  if (lower.includes("creature")) return "Creature";
  if (lower.includes("instant")) return "Instant";
  if (lower.includes("sorcery")) return "Sorcery";
  if (lower.includes("enchantment")) return "Enchantment";
  if (lower.includes("artifact")) return "Artifact";
  if (lower.includes("planeswalker")) return "Planeswalker";
  if (lower.includes("land")) return "Land";
  return "Other";
}

const TYPE_ORDER: Record<string, number> = {
  Creature: 0, Instant: 1, Sorcery: 2, Enchantment: 3,
  Artifact: 4, Planeswalker: 5, Land: 6, Other: 7,
};

interface CardGroup {
  label: string;
  cards: DraftCardInstance[];
}

function sortWithinGroup(cards: DraftCardInstance[]): DraftCardInstance[] {
  return [...cards].sort((a, b) => a.cmc - b.cmc || a.name.localeCompare(b.name));
}

function groupByColor(pool: DraftCardInstance[]): CardGroup[] {
  const groups = new Map<string, DraftCardInstance[]>();
  for (const card of pool) {
    const key = colorGroupKey(card);
    const list = groups.get(key) ?? [];
    list.push(card);
    groups.set(key, list);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => (COLOR_GROUP_ORDER[a] ?? 99) - (COLOR_GROUP_ORDER[b] ?? 99))
    .map(([key, cards]) => ({
      label: COLOR_GROUP_LABELS[key] ?? key,
      cards: sortWithinGroup(cards),
    }));
}

function groupByType(pool: DraftCardInstance[]): CardGroup[] {
  const groups = new Map<string, DraftCardInstance[]>();
  for (const card of pool) {
    const key = primaryType(card.type_line);
    const list = groups.get(key) ?? [];
    list.push(card);
    groups.set(key, list);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => (TYPE_ORDER[a] ?? 99) - (TYPE_ORDER[b] ?? 99))
    .map(([key, cards]) => ({
      label: key,
      cards: sortWithinGroup(cards),
    }));
}

function groupByCmc(pool: DraftCardInstance[]): CardGroup[] {
  const groups = new Map<string, DraftCardInstance[]>();
  for (const card of pool) {
    const key = card.cmc >= 6 ? "6+" : String(card.cmc);
    const list = groups.get(key) ?? [];
    list.push(card);
    groups.set(key, list);
  }
  const cmcOrder = ["0", "1", "2", "3", "4", "5", "6+"];
  return cmcOrder
    .filter((k) => groups.has(k))
    .map((key) => ({
      label: `${key} CMC`,
      cards: [...groups.get(key)!].sort((a, b) => a.name.localeCompare(b.name)),
    }));
}

function groupPool(pool: DraftCardInstance[], mode: PoolSortMode): CardGroup[] {
  switch (mode) {
    case "color": return groupByColor(pool);
    case "type": return groupByType(pool);
    case "cmc": return groupByCmc(pool);
  }
}

// ── Rarity badge ────────────────────────────────────────────────────────

const RARITY_DOT: Record<string, string> = {
  mythic: "bg-amber-400",
  rare: "bg-yellow-300",
  uncommon: "bg-gray-300",
  common: "bg-gray-500",
};

function rarityDotClass(rarity: string): string {
  return RARITY_DOT[rarity.toLowerCase()] ?? "bg-gray-500";
}

// ── Sort mode tabs ──────────────────────────────────────────────────────

const SORT_MODES: Array<{ mode: PoolSortMode; label: string }> = [
  { mode: "color", label: "Color" },
  { mode: "type", label: "Type" },
  { mode: "cmc", label: "CMC" },
];

// ── Component ───────────────────────────────────────────────────────────

/** Collapsible side panel showing drafted pool. Per D-07: sortable by color/type/CMC. */
export function PoolPanel() {
  const view = useDraftStore((s) => s.view);
  const poolSortMode = useDraftStore((s) => s.poolSortMode);
  const poolPanelOpen = useDraftStore((s) => s.poolPanelOpen);
  const setPoolSortMode = useDraftStore((s) => s.setPoolSortMode);
  const togglePoolPanel = useDraftStore((s) => s.togglePoolPanel);

  const pool = useMemo(() => view?.pool ?? [], [view?.pool]);

  const groups = useMemo(
    () => groupPool(pool, poolSortMode),
    [pool, poolSortMode],
  );

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-gray-700">
        <button
          onClick={togglePoolPanel}
          className="flex items-center gap-2 text-sm text-gray-300 hover:text-white transition-colors"
        >
          <span className={`transition-transform ${poolPanelOpen ? "rotate-0" : "-rotate-90"}`}>
            ▼
          </span>
          <span className="font-medium">{pool.length} cards drafted</span>
        </button>
      </div>

      {!poolPanelOpen && null}

      {poolPanelOpen && (
        <>
          {/* Sort tabs */}
          <div className="flex gap-1 px-3 py-2 border-b border-gray-700/50">
            {SORT_MODES.map(({ mode, label }) => (
              <button
                key={mode}
                onClick={() => setPoolSortMode(mode)}
                className={`px-2.5 py-1 rounded text-xs font-medium transition-colors ${
                  poolSortMode === mode
                    ? "bg-gray-600 text-white"
                    : "text-gray-400 hover:text-gray-200 hover:bg-gray-700/50"
                }`}
              >
                {label}
              </button>
            ))}
          </div>

          {/* Card groups */}
          <div className="flex-1 overflow-y-auto px-3 py-2 space-y-3">
            {groups.map((group) => (
              <div key={group.label}>
                <div className="text-[10px] font-semibold uppercase tracking-wider text-gray-500 mb-1">
                  {group.label} ({group.cards.length})
                </div>
                <div className="space-y-0.5">
                  {group.cards.map((card) => (
                    <div
                      key={card.instance_id}
                      className="flex items-center gap-2 px-2 py-1 rounded text-xs hover:bg-gray-700/50 transition-colors"
                    >
                      <span className={`w-2 h-2 rounded-full shrink-0 ${rarityDotClass(card.rarity)}`} />
                      <span className="text-gray-200 truncate">{card.name}</span>
                      <span className="text-gray-500 ml-auto shrink-0">{card.cmc}</span>
                    </div>
                  ))}
                </div>
              </div>
            ))}

            {pool.length === 0 && (
              <div className="text-gray-500 text-xs text-center py-4">
                No cards drafted yet
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}
