import type { ManaColor } from "../adapter/types";
import type { AIDifficulty } from "../constants/ai";
import type { DeckArchetype } from "../services/engineRuntime";
import type { CommanderBracketTier } from "./bracketEstimate";

export type LabelProvenance = "estimated" | "declared";

export interface BracketLabel {
  tier: CommanderBracketTier;
  provenance: LabelProvenance;
  data_version: string;
}

export type SeatAttribute = "deck" | "commander" | "color_identity";

/** Mirrors Rust's externally tagged `PodConstraint`. Do not hand-write the
 * object form: the tuple variant serializes as `{ distinct: SeatAttribute }`. */
export type PodConstraint =
  | "bracket_distance"
  | "label_provenance"
  | "coverage_floor"
  | "archetype"
  | { distinct: SeatAttribute };

export type TierEnforcement = "advisory" | "hard_gate";

export interface PodSeatOccupant {
  deck_id: string;
  commander: string[];
}

export interface PodSelectionRequest {
  allowed: CommanderBracketTier[];
  prefer: CommanderBracketTier | null;
  enforcement: TierEnforcement;
  seats: number;
  constraints: PodConstraint[];
  coverage_floor_pct: number;
  archetype: DeckArchetype | null;
  /** Crosses WASM as a JS number. Keep this in JavaScript's exact-integer
   * range; callers use one unsigned 32-bit `crypto.getRandomValues` value. */
  seed: number;
  occupied: PodSeatOccupant[];
}

/** Rust's `AiDeckCandidate`, renamed to avoid colliding with the richer
 * client catalog candidate. */
export interface AiDeckCandidateWire {
  id: string;
  commander: string[];
  label: BracketLabel | null;
  coverage_pct: number | null;
  archetype: DeckArchetype | null;
}

export interface PodSeat {
  seat_index: number;
  candidate_id: string;
  tier: CommanderBracketTier | null;
  provenance: LabelProvenance | null;
  difficulty: AIDifficulty;
  color_identity: ManaColor[];
}

export interface PodRelaxation {
  seat_index: number;
  relaxed: PodConstraint;
}

export interface PodAssignment {
  seats: PodSeat[];
  relaxations: PodRelaxation[];
}

export type PodSelectionError =
  | { insufficient_candidates: { requested: number; available: number } }
  | { hard_gate_unsatisfiable: { requested: number; available: number } }
  | { too_many_seats: { seats: number } }
  | { unknown_commander: { candidate_id: string; name: string } };

export type PodSelectionResult =
  | { ok: PodAssignment }
  | { err: PodSelectionError };

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

export function isPodSelectionResult(value: unknown): value is PodSelectionResult {
  if (!isRecord(value)) return false;
  if ("ok" in value) {
    return (
      isRecord(value.ok) &&
      Array.isArray(value.ok.seats) &&
      Array.isArray(value.ok.relaxations)
    );
  }
  if (!("err" in value) || !isRecord(value.err)) return false;
  const error = value.err;
  return [
    "insufficient_candidates",
    "hard_gate_unsatisfiable",
    "too_many_seats",
    "unknown_commander",
  ].some((key) => key in error);
}
