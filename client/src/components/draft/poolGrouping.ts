import type { DraftCardInstance } from "../../adapter/draft-adapter";
import type { PoolSortMode } from "../../stores/draftStore";

export interface DraftPoolEntry {
  card: DraftCardInstance;
  count: number;
}

export interface DraftPoolGroup {
  label: string;
  cards: DraftPoolEntry[];
}

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

function dedup(cards: DraftCardInstance[]): DraftPoolEntry[] {
  const map = new Map<string, DraftPoolEntry>();
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

function sortWithinGroup(cards: DraftCardInstance[]): DraftPoolEntry[] {
  const sorted = [...cards].sort((a, b) => a.cmc - b.cmc || a.name.localeCompare(b.name));
  return dedup(sorted);
}

function groupByColor(pool: DraftCardInstance[]): DraftPoolGroup[] {
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

export function groupDraftPoolByType(pool: DraftCardInstance[]): DraftPoolGroup[] {
  const groups = new Map<string, DraftCardInstance[]>();
  for (const card of pool) {
    const key = primaryType(card.type_line);
    const list = groups.get(key) ?? [];
    list.push(card);
    groups.set(key, list);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => (TYPE_ORDER[a] ?? 99) - (TYPE_ORDER[b] ?? 99))
    .map(([label, cards]) => ({ label, cards: sortWithinGroup(cards) }));
}

function groupByCmc(pool: DraftCardInstance[]): DraftPoolGroup[] {
  const groups = new Map<string, DraftCardInstance[]>();
  for (const card of pool) {
    const key = card.cmc >= 6 ? "6+" : String(card.cmc);
    const list = groups.get(key) ?? [];
    list.push(card);
    groups.set(key, list);
  }
  const cmcOrder = ["0", "1", "2", "3", "4", "5", "6+"];
  return cmcOrder
    .filter((key) => groups.has(key))
    .map((key) => ({
      label: `${key} CMC`,
      cards: dedup([...groups.get(key)!].sort((a, b) => a.name.localeCompare(b.name))),
    }));
}

export function groupDraftPool(pool: DraftCardInstance[], mode: PoolSortMode): DraftPoolGroup[] {
  switch (mode) {
    case "color": return groupByColor(pool);
    case "type": return groupDraftPoolByType(pool);
    case "cmc": return groupByCmc(pool);
  }
}
