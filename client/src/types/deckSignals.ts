export type DeckSignalKind =
  | "counterspells"
  | "spot_removal"
  | "sweepers"
  | "card_advantage"
  | "free_interaction"
  | "mana_producers"
  | "land_fetch"
  | "rituals"
  | "extra_land_drops";

export const DECK_SIGNAL_KINDS: readonly DeckSignalKind[] = [
  "counterspells",
  "spot_removal",
  "sweepers",
  "card_advantage",
  "free_interaction",
  "mana_producers",
  "land_fetch",
  "rituals",
  "extra_land_drops",
];

export interface DeckSignalReading {
  count: number;
  contributing: string[];
}

export interface DeckSignals {
  readings: Record<DeckSignalKind, DeckSignalReading>;
  nonland_cards: number;
  average_mana_value_centi: number | null;
  resolved_cards: number;
  unresolved_cards: number;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

function isDeckSignalReading(value: unknown): value is DeckSignalReading {
  return (
    isRecord(value) &&
    typeof value.count === "number" &&
    Array.isArray(value.contributing) &&
    value.contributing.every((card) => typeof card === "string")
  );
}

export function isDeckSignals(value: unknown): value is DeckSignals {
  if (!isRecord(value) || !isRecord(value.readings)) return false;
  const readings = value.readings;

  return (
    DECK_SIGNAL_KINDS.every((kind) => isDeckSignalReading(readings[kind])) &&
    typeof value.nonland_cards === "number" &&
    (value.average_mana_value_centi === null ||
      typeof value.average_mana_value_centi === "number") &&
    typeof value.resolved_cards === "number" &&
    typeof value.unresolved_cards === "number"
  );
}
