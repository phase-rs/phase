import type { AdditionalCost, GameAction, SerializedAbilityCost } from "../adapter/types.ts";

// Converts Rust ManaCostShard variant names to MTG abbreviations.
// This is the canonical bridge between engine serialization and display—
// ManaSymbol.tsx already works with these abbreviations ("W", "U", "W/U").
export const SHARD_ABBREVIATION: Record<string, string> = {
  White: "W", Blue: "U", Black: "B", Red: "R", Green: "G",
  Colorless: "C", Snow: "S", X: "X",
  WhiteBlue: "W/U", WhiteBlack: "W/B", BlueBlack: "U/B", BlueRed: "U/R",
  BlackRed: "B/R", BlackGreen: "B/G", RedWhite: "R/W", RedGreen: "R/G",
  GreenWhite: "G/W", GreenBlue: "G/U",
};

// Mirrors Rust AbilityCost serialization shape (serde tag = "type").
type SerializedCost = {
  type: string;
  amount?: number;
  costs?: SerializedCost[];
  cost?: { type: string; shards?: string[]; generic?: number };
};

export function formatCost(cost: SerializedCost): string {
  switch (cost.type) {
    case "Loyalty": {
      const amt = cost.amount ?? 0;
      return amt > 0 ? `+${amt}` : `${amt}`;
    }
    case "Tap": return "{T}";
    case "Untap": return "{Q}";
    case "Mana": {
      const mc = cost.cost;
      if (!mc || mc.type === "Free") return "{0}";
      const parts: string[] = [];
      if (mc.generic) parts.push(`{${mc.generic}}`);
      for (const shard of mc.shards ?? []) {
        parts.push(`{${SHARD_ABBREVIATION[shard] ?? shard}}`);
      }
      return parts.join("") || "{0}";
    }
    case "PayLife": return `${cost.amount} life`;
    case "Sacrifice": return "Sacrifice";
    case "Discard": {
      const count = (cost as { count?: number }).count ?? 1;
      return `Discard ${count} card${count > 1 ? "s" : ""}`;
    }
    case "Blight": return `Blight ${(cost as { count?: number }).count ?? 1}`;
    case "Composite":
      return (cost.costs ?? []).map(formatCost).join(", ");
    default:
      return "Activate";
  }
}

export function abilityLabel(ability: unknown): string {
  const cost = (ability as { cost?: SerializedCost } | null)?.cost;
  return cost ? formatCost(cost) : "0";
}

export function abilityChoiceLabel(action: GameAction, abilities: unknown[]): { label: string; description?: string } {
  if (action.type === "ActivateAbility") {
    const ability = abilities[action.data.ability_index] as { cost?: SerializedCost; description?: string } | null;
    const label = abilityLabel(ability);
    const description = ability?.description ? stripCostPrefix(ability.description) : undefined;
    return { label, description };
  }
  return { label: "Tap for Mana" };
}

/** Format a SerializedAbilityCost (same shape as SerializedCost but from the AdditionalCost type). */
function formatAbilityCost(cost: SerializedAbilityCost): string {
  return formatCost(cost as SerializedCost);
}

/** Build title + option labels for the OptionalCostChoice modal. */
export function additionalCostChoices(cost: AdditionalCost): { title: string; payLabel: string; skipLabel: string } {
  switch (cost.type) {
    case "Optional":
      return {
        title: `Pay additional cost: ${formatAbilityCost(cost.data)}?`,
        payLabel: `Pay ${formatAbilityCost(cost.data)}`,
        skipLabel: "Skip",
      };
    case "Choice":
      return {
        title: "Choose additional cost",
        payLabel: formatAbilityCost(cost.data[0]),
        skipLabel: formatAbilityCost(cost.data[1]),
      };
  }
}

/** Strip the leading cost prefix from Oracle text (e.g. "[+2]: Draw a card." → "Draw a card.") */
function stripCostPrefix(text: string): string {
  // Bracket format: [+2]: ..., [−1]: ..., [0]: ...
  const bracketMatch = text.match(/^\[.*?\]:\s*/);
  if (bracketMatch) return text.slice(bracketMatch[0].length);
  // Bare format: +2: ..., −1: ..., 0: ...
  const bareMatch = text.match(/^[+\-−–]?\d+:\s*/);
  if (bareMatch) return text.slice(bareMatch[0].length);
  // Mana/tap cost prefix: {T}, {2}{B}: ...
  const costMatch = text.match(/^[^:]+:\s*/);
  if (costMatch) return text.slice(costMatch[0].length);
  return text;
}
