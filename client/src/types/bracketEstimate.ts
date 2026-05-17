export type CommanderBracketTier =
  | "exhibition"
  | "core"
  | "upgraded"
  | "optimized"
  | "cedh";

export type BracketAxis =
  | "game_changers"
  | "mass_land_denial"
  | "extra_turns"
  | "efficient_tutors";

export const BRACKET_AXES: readonly BracketAxis[] = [
  "game_changers",
  "mass_land_denial",
  "extra_turns",
  "efficient_tutors",
];

export type BracketAxisCounts = Record<BracketAxis, number>;
export type BracketContributingCards = Record<BracketAxis, string[]>;
export type BracketAxisCaps = Record<BracketAxis, number | null>;

export interface BracketViolation {
  axis: BracketAxis;
  count: number;
  prior_cap: number;
  forced_floor: CommanderBracketTier;
}

export interface BracketEstimate {
  tier: CommanderBracketTier;
  axes: BracketAxisCounts;
  axis_caps_at_tier: BracketAxisCaps;
  contributing: BracketContributingCards;
  /**
   * Per-axis violations recorded for axes whose count exceeded a tier
   * ceiling. Serialized from Rust `BTreeMap<BracketAxis, BracketViolation>`
   * where a missing key means the axis stayed within bounds.
   */
  violations: Partial<Record<BracketAxis, BracketViolation>>;
  data_version: string;
}

export interface BracketDeckRequest {
  commander: string[];
  main_deck: string[];
  sideboard: string[];
}

export const BRACKET_TIER_NUMERIC: Record<CommanderBracketTier, 1 | 2 | 3 | 4 | 5> = {
  exhibition: 1,
  core: 2,
  upgraded: 3,
  optimized: 4,
  cedh: 5,
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

function hasNumberByAxis(value: unknown): value is BracketAxisCounts {
  return isRecord(value) && BRACKET_AXES.every((axis) => typeof value[axis] === "number");
}

function hasCapByAxis(value: unknown): value is BracketAxisCaps {
  return (
    isRecord(value) &&
    BRACKET_AXES.every((axis) => value[axis] === null || typeof value[axis] === "number")
  );
}

function hasStringArrayByAxis(value: unknown): value is BracketContributingCards {
  return (
    isRecord(value) &&
    BRACKET_AXES.every(
      (axis) => Array.isArray(value[axis]) && value[axis].every((card) => typeof card === "string"),
    )
  );
}

export function isBracketEstimate(value: unknown): value is BracketEstimate {
  if (!isRecord(value) || typeof value.tier !== "string") return false;
  if (!(value.tier in BRACKET_TIER_NUMERIC)) return false;
  if (typeof value.data_version !== "string") return false;
  return (
    hasNumberByAxis(value.axes) &&
    hasCapByAxis(value.axis_caps_at_tier) &&
    hasStringArrayByAxis(value.contributing) &&
    isRecord(value.violations)
  );
}
