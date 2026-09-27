import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AiDeckCandidate } from "../aiDeckCatalog";
import type { PodSelectionRequest } from "../../types/podSelection";
import { aiDeckCandidateToWire, selectPod } from "../podSelection";

const selectAiPod = vi.hoisted(() => vi.fn());

vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({ selectAiPod }),
}));

const request: PodSelectionRequest = {
  allowed: [],
  prefer: null,
  enforcement: "advisory",
  seats: 1,
  constraints: [],
  coverage_floor_pct: 0,
  archetype: null,
  seed: 7,
  occupied: [],
};

describe("selectPod", () => {
  beforeEach(() => vi.clearAllMocks());

  it("returns an assignment outcome", async () => {
    const assignment = { seats: [], relaxations: [] };
    selectAiPod.mockResolvedValue({ ok: assignment });
    await expect(selectPod([], request)).resolves.toEqual({ kind: "assignment", assignment });
  });

  it("sends only a versioned declaration with declared provenance", () => {
    const candidate: AiDeckCandidate = {
      id: "declared-deck",
      name: "Declared deck",
      source: { type: "precon", deckId: "declared-deck", code: "TST" },
      deck: {
        main: [{ name: "Forest", count: 99 }],
        sideboard: [],
        commander: ["Ezuri, Renegade Leader"],
      },
      knownFormat: "Commander",
      coveragePct: 100,
      archetype: "Midrange",
      bracket: 3,
      bracketProvenance: "declared",
      bracketDataVersion: "brackets-2026-09",
    };

    expect(aiDeckCandidateToWire(candidate).label).toEqual({
      tier: "upgraded",
      provenance: "declared",
      data_version: "brackets-2026-09",
    });
    expect(
      aiDeckCandidateToWire({ ...candidate, bracketDataVersion: null }).label,
    ).toEqual({
      tier: "upgraded",
      provenance: "estimated",
      data_version: "unverified",
    });
  });

  it("returns a refused outcome", async () => {
    const error = { too_many_seats: { seats: 5 } };
    selectAiPod.mockResolvedValue({ err: error });
    await expect(selectPod([], request)).resolves.toEqual({ kind: "refused", error });
  });

  it("returns card-data-unavailable when the adapter rejects", async () => {
    selectAiPod.mockRejectedValue(new Error("card data missing"));
    await expect(selectPod([], request)).resolves.toEqual({
      kind: "card-data-unavailable",
      reason: "card data missing",
    });
  });
});
