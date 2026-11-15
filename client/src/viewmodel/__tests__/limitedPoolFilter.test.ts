import { describe, expect, it } from "vitest";

import type { DraftPoolGroup, DraftPoolGroupKind } from "../../adapter/draft-adapter";
import {
  axisKinds,
  EMPTY_POOL_FILTER,
  poolFilterActive,
  toggleKind,
} from "../limitedPoolFilter";

// The FILTERING itself is engine-owned (`draft_core::view::filter_pool_listing`,
// #7546 review) and tested in draft-core, including the same-name/different-
// rarity regression. This file covers only the presentation state the display
// keeps: chip toggling, the "is anything asked?" predicate, and the chip
// derivation from engine-delivered groups.

function group(kind: DraftPoolGroupKind): DraftPoolGroup {
  return { kind, total: 0, cards: [] };
}

describe("toggleKind / poolFilterActive / axisKinds", () => {
  it("toggles a kind in and out", () => {
    expect(toggleKind([], "rare")).toEqual(["rare"]);
    expect(toggleKind(["rare"], "rare")).toEqual([]);
    expect(toggleKind(["rare"], "common")).toEqual(["rare", "common"]);
  });

  it("reports activity for any non-empty axis or query", () => {
    expect(poolFilterActive(EMPTY_POOL_FILTER)).toBe(false);
    expect(poolFilterActive({ ...EMPTY_POOL_FILTER, query: "  " })).toBe(false);
    expect(poolFilterActive({ ...EMPTY_POOL_FILTER, query: "a" })).toBe(true);
    expect(poolFilterActive({ ...EMPTY_POOL_FILTER, rarities: ["rare"] })).toBe(
      true,
    );
  });

  it("offers exactly the engine-delivered kinds in engine order", () => {
    expect(axisKinds([group("rare"), group("common")])).toEqual([
      "rare",
      "common",
    ]);
  });
});
