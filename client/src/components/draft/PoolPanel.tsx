import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import { useDraftStore } from "../../stores/draftStore";
import type { PoolSortMode } from "../../stores/draftStore";
import type { DraftCardInstance, DraftPlayerView } from "../../adapter/draft-adapter";
import type { CardHoverInfo } from "../card/CardPreview";

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

interface PoolEntry {
  card: DraftCardInstance;
  count: number;
}

interface CardGroup {
  label: string;
  cards: PoolEntry[];
}

function dedup(cards: DraftCardInstance[]): PoolEntry[] {
  const map = new Map<string, PoolEntry>();
  for (const card of cards) {
    const existing = map.get(card.name);
    if (existing) {
      existing.count++;
    } else {
      map.set(card.name, { card, count: 1 });
    }
  }
  return [...map.values()];
}

function sortWithinGroup(cards: DraftCardInstance[]): PoolEntry[] {
  const sorted = [...cards].sort((a, b) => a.cmc - b.cmc || a.name.localeCompare(b.name));
  return dedup(sorted);
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
      cards: dedup([...groups.get(key)!].sort((a, b) => a.name.localeCompare(b.name))),
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
  uncommon: "bg-slate-300",
  common: "bg-slate-500",
};

function rarityDotClass(rarity: string): string {
  return RARITY_DOT[rarity.toLowerCase()] ?? "bg-slate-500";
}

// ── Color pips ──────────────────────────────────────────────────────────

const COLOR_PIP: Record<string, string> = {
  W: "bg-amber-100",
  U: "bg-blue-400",
  B: "bg-purple-400",
  R: "bg-red-400",
  G: "bg-green-400",
};

function ColorPips({ colors }: { colors: string[] }) {
  if (colors.length === 0) {
    return <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-slate-500" />;
  }
  return (
    <span className="flex shrink-0 gap-0.5">
      {colors.map((c) => (
        <span
          key={c}
          className={`h-1.5 w-1.5 rounded-full ${COLOR_PIP[c] ?? "bg-slate-500"}`}
        />
      ))}
    </span>
  );
}

// ── Sort mode tabs ──────────────────────────────────────────────────────

const SORT_MODES: Array<{ mode: PoolSortMode; labelKey: string }> = [
  { mode: "color", labelKey: "pool.sortColor" },
  { mode: "type", labelKey: "pool.sortType" },
  { mode: "cmc", labelKey: "pool.sortCmc" },
];

// ── Component ───────────────────────────────────────────────────────────

interface PoolPanelProps {
  onCardHover?: (info: CardHoverInfo | null) => void;
  view?: DraftPlayerView | null;
}

export function PoolPanel({ onCardHover, view: viewOverride }: PoolPanelProps = {}) {
  const { t } = useTranslation("draft");
  const quickView = useDraftStore((s) => s.view);
  const poolSortMode = useDraftStore((s) => s.poolSortMode);
  const poolPanelOpen = useDraftStore((s) => s.poolPanelOpen);
  const setPoolSortMode = useDraftStore((s) => s.setPoolSortMode);
  const togglePoolPanel = useDraftStore((s) => s.togglePoolPanel);
  const view = viewOverride !== undefined ? viewOverride : quickView;

  const pool = useMemo(() => view?.pool ?? [], [view?.pool]);

  const groups = useMemo(
    () => groupPool(pool, poolSortMode),
    [pool, poolSortMode],
  );

  // WUBRG pool tally — a multicolor card counts toward each of its colors
  // (standard draft-tracker convention), so the strip reads as "how deep am I
  // in each color" rather than a mono-only count.
  const colorCounts = useMemo(() => {
    const counts: Record<string, number> = { W: 0, U: 0, B: 0, R: 0, G: 0 };
    for (const card of pool) {
      for (const color of card.colors) {
        if (color in counts) counts[color]++;
      }
    }
    return counts;
  }, [pool]);

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-white/10 px-3 py-2">
        <button
          onClick={togglePoolPanel}
          className="flex items-center gap-2 text-sm text-white/60 transition-colors hover:text-white"
        >
          <span className={`transition-transform ${poolPanelOpen ? "rotate-0" : "-rotate-90"}`}>
            ▼
          </span>
          <span className="font-medium">{t("pool.cardsDrafted", { count: pool.length })}</span>
        </button>
      </div>

      {!poolPanelOpen && null}

      {poolPanelOpen && (
        <>
          {/* WUBRG color-count strip (design mockup): how deep the pool is in
              each color. */}
          <div className="grid grid-cols-5 gap-1.5 border-b border-white/8 px-3 py-2">
            {(["W", "U", "B", "R", "G"] as const).map((c) => (
              <div key={c} className="flex flex-col items-center gap-1 rounded-[8px] bg-black/24 py-1.5">
                <span className={`h-3 w-3 rounded-full ${COLOR_PIP[c]} shadow-[inset_0_0_0_1px_rgba(0,0,0,0.3)]`} />
                <span className={`font-mono text-[11px] tabular-nums ${colorCounts[c] ? "text-slate-300" : "text-slate-600"}`}>
                  {colorCounts[c]}
                </span>
              </div>
            ))}
          </div>

          {/* Sort tabs */}
          <div className="flex gap-1 border-b border-white/8 px-3 py-2">
            {SORT_MODES.map(({ mode, labelKey }) => (
              <button
                key={mode}
                onClick={() => setPoolSortMode(mode)}
                className={`rounded-[12px] px-2.5 py-1 text-xs font-medium transition-colors ${
                  poolSortMode === mode
                    ? "bg-white/10 text-white"
                    : "text-white/40 hover:bg-white/5 hover:text-white/70"
                }`}
              >
                {t(labelKey)}
              </button>
            ))}
          </div>

          {/* Card groups */}
          <div className="flex-1 space-y-3 overflow-y-auto px-3 py-2">
            {groups.map((group) => (
              <div key={group.label}>
                <div className="mb-1 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
                  {group.label} ({group.cards.length})
                </div>
                <div className="space-y-0.5">
                  {group.cards.map(({ card, count }) => (
                    <div
                      key={card.instance_id}
                      onMouseEnter={onCardHover ? () => onCardHover({ name: card.name, sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number } }) : undefined}
                      onMouseLeave={onCardHover ? () => onCardHover(null) : undefined}
                      className="flex items-center gap-2 rounded-[10px] px-2 py-1 text-xs transition-colors hover:bg-white/5"
                    >
                      <span className={`h-2 w-2 shrink-0 rounded-full ${rarityDotClass(card.rarity)}`} />
                      {count > 1 && (
                        <span className="flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full bg-white/10 px-1 text-[10px] font-medium text-white/60">
                          {count}
                        </span>
                      )}
                      <span className="truncate text-white/80">{card.name}</span>
                      <span className="ml-auto flex shrink-0 items-center gap-1.5">
                        <ColorPips colors={card.colors} />
                        <span className="text-white/30">{card.cmc}</span>
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            ))}

            {pool.length === 0 && (
              <div className="py-4 text-center text-xs text-white/30">
                {t("pool.empty")}
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}
