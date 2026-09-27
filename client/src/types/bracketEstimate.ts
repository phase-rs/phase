export type CommanderBracketTier =
  | "exhibition"
  | "core"
  | "upgraded"
  | "optimized"
  | "cedh";

export type ComboWindow = "early_game" | "late_game";

/** Mirrors the engine's `ComboDeclaration` (internally tagged on `kind`). */
export type ComboDeclaration =
  | { kind: "undeclared" }
  | { kind: "none_intended" }
  | { kind: "intended"; window: ComboWindow | null };

export const UNDECLARED_COMBO: ComboDeclaration = { kind: "undeclared" };

export type Barometer =
  | "game_changers"
  | "extra_turns"
  | "mass_land_denial"
  | "two_card_combos";

export const BAROMETERS: readonly Barometer[] = [
  "game_changers",
  "extra_turns",
  "mass_land_denial",
  "two_card_combos",
];

export type BarometerAuthority = "engine" | "deck_owner" | "unanswered";

export interface ComboBarometer {
  declaration: ComboDeclaration;
  floor: CommanderBracketTier | null;
}

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
  /** Absent on payloads older than 61-08; the engine serde-defaults them. */
  combo_pairs?: [string, string][];
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

export type ComboRelevance = "helper" | "contextual" | "standalone";
export type ComboSetup = "as_printed" | "notable_prerequisites";
export type ComboPieceZone = "library" | "command_zone" | "anywhere";
export type ComboResource =
  | "mana"
  | "damage"
  | "life_loss"
  | "lifegain"
  | "mill"
  | "draw"
  | "tokens"
  | "combat"
  | "turns";
export type ComboOutcome =
  | { kind: "wins" }
  | { kind: "unbounded"; resource: ComboResource };
export type ComboCoverage = "unmeasured" | "measured";
export type ComboCardinality = "definitely_two_card" | "arguably_two_card" | "more";
export type EarlyComboReading = "definitely_only" | "including_arguable";
export type SignalSource = "card_name" | "combo_pair";

export interface ComboPiece {
  key: string;
  display: string;
  zone: ComboPieceZone;
}

export interface ComboMatch {
  pieces: [ComboPiece, ComboPiece];
  relevance: ComboRelevance;
  cardinality: ComboCardinality;
  assemble_cost: number;
  popularity: number;
  outcomes: ComboOutcome[];
  axes: BracketAxis[];
  source: SignalSource;
}

export type ComboFloorTrigger =
  | { kind: "standalone_two_card" }
  | {
      kind: "early_two_card";
      reading: EarlyComboReading;
      assemble_ceiling: number;
    };

export interface ComboCheck {
  trigger: ComboFloorTrigger;
  floor: CommanderBracketTier;
  outcome: BracketCheckOutcome;
  official_line: string;
  source_document: string;
  source_published: string;
  source_url: string;
  evidence: ComboMatch[];
}

export interface ComboFilterCounts {
  not_commander_legal: number;
  not_ok: number;
  template: number;
  one_card: number;
  three_or_more: number;
  unknown_card: number;
  irrelevant: number;
  kept: number;
  multi_zone_pieces: number;
  duplicate_pair: number;
}

export type ComboOmission =
  | "prerequisite_text"
  | "result_text"
  | "unmodeled_result_classes";

export interface ComboProvenance {
  snapshot_date: string;
  table_version: string;
  attribution: string;
  card_pool_version: string;
  filtered: ComboFilterCounts;
  omitted: ComboOmission[];
}

export type EstimateConfidence = "complete" | "partial";

export interface BracketCoverage {
  counted: number;
  resolved: number;
  unresolved: string[];
  confidence: EstimateConfidence;
}

export type DeclarationVerdict =
  | { kind: "at_or_above_floor" }
  | {
      kind: "below_floor";
      floor: CommanderBracketTier;
      raised_by: BracketAxis[];
      /** Absent on payloads older than 61-08; the engine serde-defaults them. */
      raised_by_combo_floor?: ComboFloorTrigger | null;
    };

export interface BracketEstimate {
  tier: CommanderBracketTier;
  axes: BracketAxisReadings;
  /** One row per engine floor rule, fired or not, in table order. */
  checks: BracketCheck[];
  coverage: BracketCoverage;
  data_version: string;
  /** Engine verdict on the player's declaration; `null` when none was sent. */
  declaration: DeclarationVerdict | null;
  combo_barometer: ComboBarometer;
  barometers: Partial<Record<Barometer, BarometerAuthority>>;
  /** Absent on payloads older than 61-08; the engine serde-defaults them. */
  combos?: ComboMatch[];
  /** Absent on payloads older than 61-08; the engine serde-defaults them. */
  combo_checks?: ComboCheck[];
  /** Absent on payloads older than 61-08; the engine serde-defaults them. */
  combo_coverage?: ComboCoverage;
  /** Absent on payloads older than 61-08; the engine serde-defaults them. */
  combo_provenance?: ComboProvenance | null;
}

// Mirrors the sections `estimate_bracket` counts, plus `sideboard`, which the
// engine ignores but which every caller already sends.
export interface BracketDeckRequest {
  commander: string[];
  main_deck: string[];
  sideboard: string[];
  companion: string[];
  signature_spell: string[];
  combo_declaration: ComboDeclaration;
}

export interface BracketEstimateRequest {
  deck: BracketDeckRequest;
  /** `null` = undeclared. Never omit the field — see the Rust doc comment on
   * `BracketEstimateRequest::declared_tier`. */
  declared_tier: CommanderBracketTier | null;
}

export const BRACKET_TIER_NUMERIC: Record<CommanderBracketTier, 1 | 2 | 3 | 4 | 5> = {
  exhibition: 1,
  core: 2,
  upgraded: 3,
  optimized: 4,
  cedh: 5,
};

/** Inverse of `BRACKET_TIER_NUMERIC`. Pure representation mapping between the
 * picker's numeric form and the engine's tier enum — no derivation. */
export const BRACKET_TIER_BY_NUMERIC: Record<1 | 2 | 3 | 4 | 5, CommanderBracketTier> = {
  1: "exhibition",
  2: "core",
  3: "upgraded",
  4: "optimized",
  5: "cedh",
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

function isAxisReading(value: unknown): value is AxisReading {
  return (
    isRecord(value) &&
    typeof value.count === "number" &&
    Array.isArray(value.contributing) &&
    value.contributing.every((card) => typeof card === "string") &&
    (value.combo_pairs === undefined || isStringPairArray(value.combo_pairs))
  );
}

function hasReadingByAxis(value: unknown): value is BracketAxisReadings {
  return isRecord(value) && BRACKET_AXES.every((axis) => isAxisReading(value[axis]));
}

function isTier(value: unknown): value is CommanderBracketTier {
  return typeof value === "string" && value in BRACKET_TIER_NUMERIC;
}

export function isComboDeclaration(value: unknown): value is ComboDeclaration {
  if (!isRecord(value)) return false;
  switch (value.kind) {
    case "undeclared":
    case "none_intended":
      return true;
    case "intended":
      return (
        value.window === undefined ||
        value.window === null ||
        value.window === "early_game" ||
        value.window === "late_game"
      );
    default:
      return false;
  }
}

function isBarometerAuthority(value: unknown): value is BarometerAuthority {
  return value === "engine" || value === "deck_owner" || value === "unanswered";
}

function isComboBarometer(value: unknown): value is ComboBarometer {
  return (
    isRecord(value) &&
    isComboDeclaration(value.declaration) &&
    (value.floor === null || isTier(value.floor))
  );
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isStringPairArray(value: unknown): value is [string, string][] {
  return (
    Array.isArray(value) &&
    value.every(
      (pair) =>
        Array.isArray(pair) &&
        pair.length === 2 &&
        typeof pair[0] === "string" &&
        typeof pair[1] === "string",
    )
  );
}

const COMBO_RELEVANCES: readonly ComboRelevance[] = ["helper", "contextual", "standalone"];
const COMBO_PIECE_ZONES: readonly ComboPieceZone[] = ["library", "command_zone", "anywhere"];
const COMBO_RESOURCES: readonly ComboResource[] = [
  "mana",
  "damage",
  "life_loss",
  "lifegain",
  "mill",
  "draw",
  "tokens",
  "combat",
  "turns",
];
const COMBO_CARDINALITIES: readonly ComboCardinality[] = [
  "definitely_two_card",
  "arguably_two_card",
  "more",
];
const EARLY_COMBO_READINGS: readonly EarlyComboReading[] = [
  "definitely_only",
  "including_arguable",
];
const SIGNAL_SOURCES: readonly SignalSource[] = ["card_name", "combo_pair"];
const COMBO_OMISSIONS: readonly ComboOmission[] = [
  "prerequisite_text",
  "result_text",
  "unmodeled_result_classes",
];

function isBracketAxis(value: unknown): value is BracketAxis {
  return typeof value === "string" && BRACKET_AXES.includes(value as BracketAxis);
}

function isComboFloorTrigger(value: unknown): value is ComboFloorTrigger {
  if (!isRecord(value)) return false;
  if (value.kind === "standalone_two_card") return true;
  return (
    value.kind === "early_two_card" &&
    typeof value.reading === "string" &&
    EARLY_COMBO_READINGS.includes(value.reading as EarlyComboReading) &&
    typeof value.assemble_ceiling === "number"
  );
}

function isComboPiece(value: unknown): value is ComboPiece {
  return (
    isRecord(value) &&
    typeof value.key === "string" &&
    typeof value.display === "string" &&
    typeof value.zone === "string" &&
    COMBO_PIECE_ZONES.includes(value.zone as ComboPieceZone)
  );
}

function isComboOutcome(value: unknown): value is ComboOutcome {
  if (!isRecord(value)) return false;
  if (value.kind === "wins") return true;
  return (
    value.kind === "unbounded" &&
    typeof value.resource === "string" &&
    COMBO_RESOURCES.includes(value.resource as ComboResource)
  );
}

function isComboMatch(value: unknown): value is ComboMatch {
  return (
    isRecord(value) &&
    Array.isArray(value.pieces) &&
    value.pieces.length === 2 &&
    value.pieces.every(isComboPiece) &&
    typeof value.relevance === "string" &&
    COMBO_RELEVANCES.includes(value.relevance as ComboRelevance) &&
    typeof value.cardinality === "string" &&
    COMBO_CARDINALITIES.includes(value.cardinality as ComboCardinality) &&
    typeof value.assemble_cost === "number" &&
    typeof value.popularity === "number" &&
    Array.isArray(value.outcomes) &&
    value.outcomes.every(isComboOutcome) &&
    Array.isArray(value.axes) &&
    value.axes.every(isBracketAxis) &&
    typeof value.source === "string" &&
    SIGNAL_SOURCES.includes(value.source as SignalSource)
  );
}

function isComboCheck(value: unknown): value is ComboCheck {
  return (
    isRecord(value) &&
    isComboFloorTrigger(value.trigger) &&
    isTier(value.floor) &&
    isCheckOutcome(value.outcome) &&
    typeof value.official_line === "string" &&
    typeof value.source_document === "string" &&
    typeof value.source_published === "string" &&
    typeof value.source_url === "string" &&
    Array.isArray(value.evidence) &&
    value.evidence.every(isComboMatch)
  );
}

function isComboFilterCounts(value: unknown): value is ComboFilterCounts {
  return (
    isRecord(value) &&
    [
      "not_commander_legal",
      "not_ok",
      "template",
      "one_card",
      "three_or_more",
      "unknown_card",
      "irrelevant",
      "kept",
      "multi_zone_pieces",
      "duplicate_pair",
    ].every((key) => typeof value[key] === "number")
  );
}

function isComboProvenance(value: unknown): value is ComboProvenance {
  return (
    isRecord(value) &&
    typeof value.snapshot_date === "string" &&
    typeof value.table_version === "string" &&
    typeof value.attribution === "string" &&
    typeof value.card_pool_version === "string" &&
    isComboFilterCounts(value.filtered) &&
    Array.isArray(value.omitted) &&
    value.omitted.every(
      (omission) =>
        typeof omission === "string" && COMBO_OMISSIONS.includes(omission as ComboOmission),
    )
  );
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

function isDeclarationVerdict(value: unknown): value is DeclarationVerdict {
  if (!isRecord(value)) return false;
  if (value.kind === "at_or_above_floor") return true;
  return (
    value.kind === "below_floor" &&
    isTier(value.floor) &&
    Array.isArray(value.raised_by) &&
    value.raised_by.every(isBracketAxis) &&
    (value.raised_by_combo_floor === undefined ||
      value.raised_by_combo_floor === null ||
      isComboFloorTrigger(value.raised_by_combo_floor))
  );
}

export function isBracketEstimate(value: unknown): value is BracketEstimate {
  if (!isRecord(value) || !isTier(value.tier)) return false;
  if (typeof value.data_version !== "string") return false;
  return (
    hasReadingByAxis(value.axes) &&
    Array.isArray(value.checks) &&
    value.checks.every(isBracketCheck) &&
    isCoverage(value.coverage) &&
    (value.declaration === undefined ||
      value.declaration === null ||
      isDeclarationVerdict(value.declaration)) &&
    (value.combo_barometer === undefined || isComboBarometer(value.combo_barometer)) &&
    (value.barometers === undefined ||
      (isRecord(value.barometers) &&
        Object.values(value.barometers).every(isBarometerAuthority))) &&
    (value.combos === undefined ||
      (Array.isArray(value.combos) && value.combos.every(isComboMatch))) &&
    (value.combo_checks === undefined ||
      (Array.isArray(value.combo_checks) && value.combo_checks.every(isComboCheck))) &&
    (value.combo_coverage === undefined ||
      value.combo_coverage === "unmeasured" ||
      value.combo_coverage === "measured") &&
    (value.combo_provenance === undefined ||
      value.combo_provenance === null ||
      isComboProvenance(value.combo_provenance))
  );
}
