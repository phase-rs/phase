import { beforeEach, describe, expect, it, vi } from "vitest";

import { getSharedAdapter } from "../../adapter/wasm-adapter";
import type { BracketEstimate, BracketEstimateRequest } from "../../types/bracketEstimate";
import { estimateDeckBracket } from "../bracketEstimate";

vi.mock("../../adapter/wasm-adapter", () => ({ getSharedAdapter: vi.fn() }));

const estimate: BracketEstimate = {
  tier: "core",
  axes: {
    game_changers: { count: 0, contributing: [] },
    mass_land_denial: { count: 0, contributing: [] },
    extra_turns: { count: 0, contributing: [] },
    efficient_tutors: { count: 0, contributing: [] },
  },
  checks: [],
  coverage: { counted: 1, resolved: 1, unresolved: [], confidence: "complete" },
  data_version: "test-1",
  declaration: null,
};

const request: BracketEstimateRequest = {
  deck: {
    commander: ["Atraxa, Praetors' Voice"],
    main_deck: ["Forest"],
    sideboard: [],
    companion: [],
    signature_spell: [],
  },
  declared_tier: null,
};

const estimateBracket = vi.fn();

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(getSharedAdapter).mockReturnValue({
    estimateBracket,
  } as unknown as ReturnType<typeof getSharedAdapter>);
});

describe("estimateDeckBracket", () => {
  it("returns the estimate outcome", async () => {
    estimateBracket.mockResolvedValue(estimate);
    await expect(estimateDeckBracket(request)).resolves.toEqual({ kind: "estimate", estimate });
    expect(estimateBracket).toHaveBeenCalledWith(request);
  });

  it("maps a null estimate to no-commander", async () => {
    estimateBracket.mockResolvedValue(null);
    await expect(estimateDeckBracket(request)).resolves.toEqual({ kind: "no-commander" });
  });

  it("maps a rejection to card-data-unavailable with its message", async () => {
    estimateBracket.mockRejectedValue(new Error("card database unavailable"));
    await expect(estimateDeckBracket(request)).resolves.toEqual({
      kind: "card-data-unavailable",
      reason: "card database unavailable",
    });
  });
});
