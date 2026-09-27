import type { TFunction } from "i18next";

import { getSharedAdapter } from "../adapter/wasm-adapter";
import type {
  AiDeckCandidateWire,
  PodAssignment,
  PodConstraint,
  PodSelectionError,
  PodSelectionRequest,
} from "../types/podSelection";
import type { AiDeckCandidate } from "./aiDeckCatalog";
import { expandParsedDeck } from "./deckParser";
import { BRACKET_TIER_BY_NUMERIC } from "../types/bracketEstimate";

export const POD_SELECTION_CONSTRAINTS = [
  { distinct: "deck" },
  { distinct: "commander" },
  { distinct: "color_identity" },
  "bracket_distance",
  "label_provenance",
  "coverage_floor",
  "archetype",
] as const satisfies PodSelectionRequest["constraints"];

export function podSelectionSeed(): number {
  return crypto.getRandomValues(new Uint32Array(1))[0];
}

export function aiDeckCandidateToWire(candidate: AiDeckCandidate): AiDeckCandidateWire {
  const commander = expandParsedDeck(candidate.deck).commander;
  const hasCurrentDeclaration =
    candidate.bracketProvenance === "declared" && candidate.bracketDataVersion !== null;
  return {
    id: candidate.id,
    commander,
    label:
      candidate.bracket === null || candidate.bracketProvenance === null
        ? null
        : {
            tier: BRACKET_TIER_BY_NUMERIC[candidate.bracket],
            // A declaration is authoritative only when the catalog observed
            // the engine's current data version. Without it, the client cannot
            // vouch for the declaration and must submit it as an estimate.
            provenance: hasCurrentDeclaration ? "declared" : "estimated",
            data_version: candidate.bracketDataVersion ?? "unverified",
          },
    coverage_pct: candidate.coveragePct,
    archetype: candidate.archetype,
  };
}

export type PodSelectionOutcome =
  | { kind: "assignment"; assignment: PodAssignment }
  | { kind: "refused"; error: PodSelectionError }
  | { kind: "card-data-unavailable"; reason: string };

export type PodRelaxationI18nKey =
  | "bracketDistance"
  | "labelProvenance"
  | "archetype"
  | "distinctColorIdentity"
  | "distinctCommander";

/** Maps every wire constraint explicitly; known non-relaxable constraints have no UI row. */
export function podConstraintI18nKey(
  constraint: PodConstraint,
): PodRelaxationI18nKey | null {
  if (typeof constraint === "string") {
    switch (constraint) {
      case "bracket_distance":
        return "bracketDistance";
      case "label_provenance":
        return "labelProvenance";
      case "coverage_floor":
        return null;
      case "archetype":
        return "archetype";
    }
  }

  switch (constraint.distinct) {
    case "deck":
      return null;
    case "commander":
      return "distinctCommander";
    case "color_identity":
      return "distinctColorIdentity";
  }
}

export function podSelectionErrorMessage(t: TFunction, error: PodSelectionError): string {
  if ("insufficient_candidates" in error) {
    return t(
      "menu:gameProvider.podSelectionRefused.insufficientCandidates",
      error.insufficient_candidates,
    );
  }
  if ("hard_gate_unsatisfiable" in error) {
    return t(
      "menu:gameProvider.podSelectionRefused.hardGateUnsatisfiable",
      error.hard_gate_unsatisfiable,
    );
  }
  if ("too_many_seats" in error) {
    return t("menu:gameProvider.podSelectionRefused.tooManySeats", error.too_many_seats);
  }
  return t(
    "menu:gameProvider.podSelectionRefused.unknownCommander",
    error.unknown_commander,
  );
}

/** Stateless pre-game pod selection through the shared local engine. */
export async function selectPod(
  candidates: AiDeckCandidateWire[],
  request: PodSelectionRequest,
): Promise<PodSelectionOutcome> {
  try {
    const result = await getSharedAdapter().selectAiPod(candidates, request);
    if ("ok" in result) return { kind: "assignment", assignment: result.ok };
    return { kind: "refused", error: result.err };
  } catch (error) {
    return {
      kind: "card-data-unavailable",
      reason: error instanceof Error ? error.message : String(error),
    };
  }
}
