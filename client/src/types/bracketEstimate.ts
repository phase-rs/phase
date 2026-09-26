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
  contributing: string[];
}

/** Total, not Partial: the engine emits every BracketAxis, including zero-count axes. */
export type BracketAxisReadings = Record<BracketAxis, AxisReading>;

export type BracketCheckOutcome =
  | { kind: "clear"; cards_until_fired: number | null }
  | { kind: "fired" };

export interface BracketCheck {
  axis: BracketAxis;
  comparator: "GT" | "LT" | "GE" | "LE" | "EQ" | "NE";
  threshold: number;
  floor: CommanderBracketTier;
  observed: number;
  outcome: BracketCheckOutcome;
  official_line: string;
  source_document: string;
  source_published: string;
  source_url: string;
  evidence: string[];
}

export type EstimateConfidence = "complete" | "partial";

export interface BracketCoverage {
  counted: number;
  resolved: number;
  unresolved: string[];
  confidence: EstimateConfidence;
}

export interface BracketEstimate {
  tier: CommanderBracketTier;
  axes: BracketAxisReadings;
  /** One row per engine floor rule, fired or not, in table order. */
  checks: BracketCheck[];
  coverage: BracketCoverage;
  data_version: string;
}

// Mirrors the sections `estimate_bracket` counts, plus `sideboard`, which the
// engine ignores but which every caller already sends.
export interface BracketDeckRequest {
  commander: string[];
  main_deck: string[];
  sideboard: string[];
  companion: string[];
  signature_spell: string[];
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
    Array.isArray(value.contributing) &&
    value.contributing.every((card) => typeof card === "string")
  );
}

function hasReadingByAxis(value: unknown): value is BracketAxisReadings {
  return isRecord(value) && BRACKET_AXES.every((axis) => isAxisReading(value[axis]));
}

function isTier(value: unknown): value is CommanderBracketTier {
  return typeof value === "string" && value in BRACKET_TIER_NUMERIC;
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isCheckOutcome(value: unknown): value is BracketCheckOutcome {
  if (!isRecord(value) || (value.kind !== "clear" && value.kind !== "fired")) return false;
  return (
    value.kind === "fired" ||
    value.cards_until_fired === null ||
    typeof value.cards_until_fired === "number"
  );
}

function isBracketCheck(value: unknown): value is BracketCheck {
  return (
    isRecord(value) &&
    typeof value.axis === "string" &&
    BRACKET_AXES.includes(value.axis as BracketAxis) &&
    typeof value.comparator === "string" &&
    ["GT", "LT", "GE", "LE", "EQ", "NE"].includes(value.comparator) &&
    typeof value.observed === "number" &&
    typeof value.threshold === "number" &&
    isTier(value.floor) &&
    isCheckOutcome(value.outcome) &&
    typeof value.official_line === "string" &&
    typeof value.source_document === "string" &&
    typeof value.source_published === "string" &&
    typeof value.source_url === "string" &&
    isStringArray(value.evidence)
  );
}

function isCoverage(value: unknown): value is BracketCoverage {
  return (
    isRecord(value) &&
    typeof value.counted === "number" &&
    typeof value.resolved === "number" &&
    isStringArray(value.unresolved) &&
    (value.confidence === "complete" || value.confidence === "partial")
  );
}

export function isBracketEstimate(value: unknown): value is BracketEstimate {
  if (!isRecord(value) || !isTier(value.tier)) return false;
  if (typeof value.data_version !== "string") return false;
  return (
    hasReadingByAxis(value.axes) &&
    Array.isArray(value.checks) &&
    value.checks.every(isBracketCheck) &&
    isCoverage(value.coverage)
  );
}
