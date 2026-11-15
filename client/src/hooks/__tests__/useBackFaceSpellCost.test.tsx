import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { ManaCost } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useBackFaceSpellCost } from "../useBackFaceSpellCost.ts";

const printedBack: ManaCost = { type: "Cost", shards: ["Green"], generic: 3 };

function seed(
  backFaceSpellCosts: Record<string, ManaCost> | undefined,
  spellCosts: Record<string, ManaCost> = { "7": { type: "Cost", shards: ["Green"], generic: 2 } },
) {
  useGameStore.setState({
    gameState: { derived: { back_face_spell_costs: backFaceSpellCosts } } as never,
    spellCosts,
  });
}

afterEach(() => {
  cleanup();
  useGameStore.setState({ gameState: null, spellCosts: {} });
});

describe("useBackFaceSpellCost", () => {
  // CR 709.3 + CR 712.11b: the engine publishes the other face's live cost;
  // the hook only styles it against the printed one, as the live face's badge
  // is styled.
  it("reads the engine's second-face cost and flags a reduction against the printed cost", () => {
    seed({ "7": { type: "Cost", shards: ["Green"], generic: 1 } });

    const { result } = renderHook(() => useBackFaceSpellCost(7, printedBack));

    expect(result.current).toEqual({
      cost: { type: "Cost", shards: ["Green"], generic: 1 },
      isReduced: true,
    });
  });

  it("is silent when the engine published nothing for the object", () => {
    seed({ "8": printedBack });

    const { result } = renderHook(() => useBackFaceSpellCost(7, printedBack));

    expect(result.current).toBeUndefined();
  });

  it("is silent when the front has no live cost — replay, or outside priority", () => {
    // A printed front beside a live back would be a pair of two different
    // authorities; without `spellCosts` the badge stays single.
    seed({ "7": printedBack }, {});

    const { result } = renderHook(() => useBackFaceSpellCost(7, printedBack));

    expect(result.current).toBeUndefined();
  });

  it("is silent without a printed back-face cost to style against", () => {
    seed({ "7": printedBack });

    const { result } = renderHook(() => useBackFaceSpellCost(7, undefined));

    expect(result.current).toBeUndefined();
  });
});
