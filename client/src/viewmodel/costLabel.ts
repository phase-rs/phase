import type {
  AdditionalCost,
  GameAction,
  GameObject,
  ManaCost,
  ObjectId,
  SerializedAbility,
  SerializedAbilityCost,
} from "../adapter/types.ts";
import { getCrewPower, getSaddlePower } from "./keywordProps.ts";

// Converts Rust ManaCostShard variant names to MTG abbreviations.
// This is the canonical bridge between engine serialization and display—
// ManaSymbol.tsx already works with these abbreviations ("W", "U", "W/U").
export const SHARD_ABBREVIATION: Record<string, string> = {
  White: "W", Blue: "U", Black: "B", Red: "R", Green: "G",
  Colorless: "C", Snow: "S", X: "X",
  WhiteBlue: "W/U", WhiteBlack: "W/B", BlueBlack: "U/B", BlueRed: "U/R",
  BlackRed: "B/R", BlackGreen: "B/G", RedWhite: "R/W", RedGreen: "R/G",
  GreenWhite: "G/W", GreenBlue: "G/U",
  // CR 107.4e: Monocolored hybrid {2/C}.
  TwoWhite: "2/W", TwoBlue: "2/U", TwoBlack: "2/B", TwoRed: "2/R", TwoGreen: "2/G",
  // CR 107.4e: Colorless hybrid {C/color}.
  ColorlessWhite: "C/W", ColorlessBlue: "C/U", ColorlessBlack: "C/B",
  ColorlessRed: "C/R", ColorlessGreen: "C/G",
  // CR 107.4f: Phyrexian mana.
  PhyrexianWhite: "W/P", PhyrexianBlue: "U/P", PhyrexianBlack: "B/P",
  PhyrexianRed: "R/P", PhyrexianGreen: "G/P",
  // CR 107.4f: Hybrid Phyrexian (10 variants).
  PhyrexianWhiteBlue: "W/U/P", PhyrexianWhiteBlack: "W/B/P",
  PhyrexianBlueBlack: "U/B/P", PhyrexianBlueRed: "U/R/P",
  PhyrexianBlackRed: "B/R/P", PhyrexianBlackGreen: "B/G/P",
  PhyrexianRedWhite: "R/W/P", PhyrexianRedGreen: "R/G/P",
  PhyrexianGreenWhite: "G/W/P", PhyrexianGreenBlue: "G/U/P",
};

/** Convert a ManaCost to display-ready shard abbreviations (e.g., ["2", "U", "U"]). */
export function manaCostToShards(cost: ManaCost): string[] {
  if (cost.type !== "Cost") return [];
  const shards: string[] = [];
  if (cost.generic > 0) shards.push(String(cost.generic));
  for (const s of cost.shards) {
    shards.push(SHARD_ABBREVIATION[s] ?? s);
  }
  return shards;
}

// Mirrors Rust AbilityCost serialization shape (serde tag = "type").
// `amount`/`count` on PayLife/Discard are `QuantityExpr` (a typed enum), not
// raw numbers — the engine serializes `{ type: "Fixed", value: N }` etc.
type QuantityExpr =
  | { type: "Fixed"; value: number }
  | { type: "Ref"; qty: { type: string; [key: string]: unknown } }
  | { type: "HalfRounded"; inner: QuantityExpr; rounding: string }
  | { type: "Offset"; inner: QuantityExpr; offset: number }
  | { type: "Multiply"; factor: number; inner: QuantityExpr };

type SerializedCost = {
  type: string;
  amount?: QuantityExpr | number;
  count?: QuantityExpr | number;
  costs?: SerializedCost[];
  cost?: { type: string; shards?: string[]; generic?: number };
};

/** Render a QuantityExpr (or legacy raw number) for display in cost labels. */
function formatQuantity(q: QuantityExpr | number | undefined, fallback = 1): string {
  if (q == null) return String(fallback);
  if (typeof q === "number") return String(q);
  switch (q.type) {
    case "Fixed":
      return String(q.value);
    case "Ref":
      return formatQuantityRef(q.qty);
    case "HalfRounded": {
      const dir = q.rounding === "Down" ? "rounded down" : "rounded up";
      return `half ${formatQuantity(q.inner)} (${dir})`;
    }
    case "Offset": {
      const sign = q.offset >= 0 ? "+" : "−";
      return `${formatQuantity(q.inner)} ${sign} ${Math.abs(q.offset)}`;
    }
    case "Multiply":
      if (q.factor === -1) return `−${formatQuantity(q.inner)}`;
      if (q.factor === 2) return `twice ${formatQuantity(q.inner)}`;
      return `${q.factor}× ${formatQuantity(q.inner)}`;
  }
}

// QuantityRef → English noun phrase for inline cost labels ("Pay <q> life",
// "Discard <q> card(s)"). The engine's `QuantityRef` enum has 50+ variants;
// only the ones that actually appear in resolved-cost contexts are listed
// here. Variants not listed fall through to a humanized form of the variant
// name (e.g. "TargetPower" → "target power") rather than literal "X" — that
// keeps the UI debuggable when a new variant ships before this map is
// updated, while still flagging it as a polish gap during review.
//
// The architecturally correct long-term fix is engine-side: have the engine
// emit a `display_label` field on each `QuantityRef` payload so the frontend
// stops curating this list. Tracked as a follow-up alongside the parser-side
// `display_name` work.
function formatQuantityRef(ref: { type: string; [key: string]: unknown }): string {
  switch (ref.type) {
    // Controller-relative scalar references
    case "HandSize": return "cards in your hand";
    case "LifeTotal": return "your life total";
    case "GraveyardSize": return "cards in your graveyard";
    case "StartingLifeTotal": return "starting life total";
    case "LifeAboveStarting": return "life above your starting total";
    case "LifeLostThisTurn": return "life you lost this turn";
    case "LifeGainedThisTurn": return "life you gained this turn";
    case "OpponentLifeLostThisTurn": return "life an opponent lost this turn";
    // Source-object references
    case "SelfPower": return "this creature's power";
    case "SelfToughness": return "this creature's toughness";
    // Target-object references
    case "TargetPower": return "target's power";
    case "TargetLifeTotal": return "target's life total";
    case "AnyCountersOnTarget": return "counters on target";
    // Per-turn aggregates
    case "TurnsTaken": return "turns taken";
    case "SpellsCastThisTurn": return "spells cast this turn";
    case "SpellsCastLastTurn": return "spells cast last turn";
    case "CreaturesDiedThisTurn": return "creatures that died this turn";
    case "AttackedThisTurn": return "creatures that attacked this turn";
    case "DescendedThisTurn": return "permanents that left the battlefield this turn";
    case "PermanentsLeftBattlefieldThisTurn": return "permanents that left the battlefield this turn";
    case "NonlandPermanentsLeftBattlefieldThisTurn": return "nonland permanents that left this turn";
    case "CrimesCommittedThisTurn": return "crimes committed this turn";
    // Counter / property references
    case "BasicLandTypeCount": return "basic land types you control";
    case "PartySize": return "your party's size";
    case "TrackedSetSize": return "exiled cards";
    case "CardsExiledBySource": return "cards exiled by this";
    case "ExiledFromHandThisResolution": return "cards exiled from hand";
    case "Speed": return "your speed";
    case "ChosenNumber": return "the chosen number";
    case "PreviousEffectAmount": return "the previous amount";
    case "EventContextAmount": return "the amount";
    case "EventContextSourcePower": return "the source's power";
    case "EventContextSourceToughness": return "the source's toughness";
    case "EventContextSourceManaValue": return "the source's mana value";
    case "EventContextSourceCostX": return "the source's X";
    case "Variable": return typeof ref.name === "string" ? ref.name : "X";
    default:
      // Humanize the variant name as a debug-grade fallback. PascalCase →
      // lowercased space-separated form so a new variant looks like a sentence
      // fragment rather than a code identifier.
      return ref.type.replace(/([a-z])([A-Z])/g, "$1 $2").toLowerCase();
  }
}

/** Numeric quantity check that works against either QuantityExpr or a raw number. */
function quantityIsPlural(q: QuantityExpr | number | undefined): boolean {
  if (q == null) return false;
  if (typeof q === "number") return q > 1;
  return q.type === "Fixed" ? q.value > 1 : true;
}

export function formatCost(cost: SerializedCost): string {
  switch (cost.type) {
    case "Loyalty": {
      // CR 606.1: Loyalty cost is always a literal `i32` on the Rust side.
      const amt = (typeof cost.amount === "number" ? cost.amount : 0);
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
    case "PayLife": return `Pay ${formatQuantity(cost.amount, 1)} life`;
    case "Sacrifice": return "Sacrifice";
    case "Discard": {
      const label = formatQuantity(cost.count, 1);
      return `Discard ${label} card${quantityIsPlural(cost.count) ? "s" : ""}`;
    }
    case "Blight": return `Blight ${cost.count ?? 1}`;
    case "CollectEvidence":
      return `Collect evidence ${cost.amount ?? 0}`;
    case "Composite":
      return (cost.costs ?? []).map(formatCost).join(", ");
    default:
      return "Activate";
  }
}

export function abilityLabel(ability: SerializedAbility | null | undefined): string {
  const cost = ability?.cost;
  return cost ? formatCost(cost) : "0";
}

// Maps ManaColor names to MTG mana symbol abbreviations.
const MANA_COLOR_ABBREVIATION: Record<string, string> = {
  White: "W", Blue: "U", Black: "B", Red: "R", Green: "G",
};

export function abilityChoiceLabel(
  action: GameAction,
  object: GameObject,
  objects?: Record<ObjectId, GameObject>,
): { label: string; description?: string } {
  // CR 702.190a: Sneak — label identifies which unblocked attacker is
  // returned to pay the Sneak cost. Include the Sneak mana cost from the
  // spell's keyword metadata when available.
  if (action.type === "CastSpellAsSneak") {
    const returnedId = action.data.creature_to_return;
    const returnedName = objects?.[returnedId]?.name ?? `creature #${returnedId}`;
    const sneakKeyword = object.keywords.find(
      (k): k is { Sneak: ManaCost } => typeof k === "object" && "Sneak" in k,
    );
    const costSymbols = sneakKeyword ? manaCostToShards(sneakKeyword.Sneak).map((s) => `{${s}}`).join("") : "";
    const costSuffix = costSymbols ? ` (${costSymbols})` : "";
    return {
      label: `Sneak — return ${returnedName}${costSuffix}`,
      description: `Cast ${object.name} by paying its sneak cost and returning ${returnedName} to your hand (CR 702.190a).`,
    };
  }
  if (action.type === "ActivateAbility") {
    const ability = object.abilities[action.data.ability_index];
    // For mana abilities, show what they produce (e.g., "Add {U}") instead of just the cost
    if (ability?.effect?.type === "Mana" && ability.effect.produced) {
      const produced = ability.effect.produced;
      if (produced.type === "Fixed" && produced.colors?.length) {
        const symbols = produced.colors.map((c) => `{${MANA_COLOR_ABBREVIATION[c] ?? c}}`).join("");
        return { label: `Add ${symbols}` };
      }
      if (produced.type === "Colorless") {
        return { label: "Add {C}" };
      }
    }
    const label = abilityLabel(ability);
    const description = ability?.description ? stripCostPrefix(ability.description) : undefined;
    return { label, description };
  }
  if (action.type === "CastSpell") {
    return { label: `Cast ${object.name}` };
  }
  if (action.type === "PlayLand") {
    const landFaceName = object.card_types.core_types.includes("Land")
      ? object.name
      : object.back_face?.name ?? object.name;
    return { label: `Play ${landFaceName}`, description: "Play this card as a land" };
  }
  // CR 702.122a: Crew N — read N from the engine-provided keyword.
  if (action.type === "CrewVehicle") {
    const n = getCrewPower(object.keywords);
    return {
      label: n != null ? `Crew ${n}` : "Crew",
      description: n != null
        ? `Tap any number of other creatures you control with total power ${n} or greater.`
        : "Tap creatures to crew this Vehicle.",
    };
  }
  // CR 702.184a: Station — single-creature cost; per-creature counter count.
  if (action.type === "ActivateStation") {
    return {
      label: "Station",
      description:
        "Tap another untapped creature you control; put charge counters equal to its power on this Spacecraft.",
    };
  }
  // CR 702.171a: Saddle N.
  if (action.type === "SaddleMount") {
    const n = getSaddlePower(object.keywords);
    return {
      label: n != null ? `Saddle ${n}` : "Saddle",
      description: n != null
        ? `Tap any number of other untapped creatures you control with total power ${n} or greater.`
        : "Tap creatures to saddle this Mount.",
    };
  }
  // CR 702.6a: Equip — target a creature you control.
  if (action.type === "Equip") {
    return {
      label: "Equip",
      description: "Attach this Equipment to target creature you control.",
    };
  }
  return { label: "Tap for Mana" };
}

/** Format a SerializedAbilityCost (same shape as SerializedCost but from the AdditionalCost type). */
function formatAbilityCost(cost: SerializedAbilityCost): string {
  return formatCost(cost);
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
