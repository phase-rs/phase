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

export interface AxisReading {
  count: number;
  /** null = no cap on this axis at the resolved tier. */
  cap_at_tier: number | null;
  contributing: string[];
}

/** Total, not Partial: the engine emits every BracketAxis, including zero-count axes. */
export type BracketAxisReadings = Record<BracketAxis, AxisReading>;

export interface BracketViolation {
  axis: BracketAxis;
  count: number;
  prior_cap: number;
  forced_floor: CommanderBracketTier;
}

export interface BracketEstimate {
  tier: CommanderBracketTier;
  axes: BracketAxisReadings;
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

function isAxisReading(value: unknown): value is AxisReading {
  return (
    isRecord(value) &&
    typeof value.count === "number" &&
    (value.cap_at_tier === null || typeof value.cap_at_tier === "number") &&
    Array.isArray(value.contributing) &&
    value.contributing.every((card) => typeof card === "string")
  );
}

function hasReadingByAxis(value: unknown): value is BracketAxisReadings {
  return isRecord(value) && BRACKET_AXES.every((axis) => isAxisReading(value[axis]));
}

export function isBracketEstimate(value: unknown): value is BracketEstimate {
  if (!isRecord(value) || typeof value.tier !== "string") return false;
  if (!(value.tier in BRACKET_TIER_NUMERIC)) return false;
  if (typeof value.data_version !== "string") return false;
  return hasReadingByAxis(value.axes) && isRecord(value.violations);
}
