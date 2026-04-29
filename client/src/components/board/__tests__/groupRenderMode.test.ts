import { describe, expect, it } from "vitest";

import type { GroupedPermanent } from "../../../viewmodel/battlefieldProps.ts";
import {
  getCreatureGroupRenderMode,
  visibleCardSlotCount,
  visibleStaggerCount,
} from "../groupRenderMode.ts";

function group(count: number): GroupedPermanent {
  return {
    name: "Saproling",
    ids: Array.from({ length: count }, (_, index) => index + 1),
    count,
    representative: {} as GroupedPermanent["representative"],
  };
}

describe("getCreatureGroupRenderMode", () => {
  it("keeps one creature as a single card", () => {
    expect(getCreatureGroupRenderMode(group(1), "creatures", {
      manualExpanded: false,
      containsCommittedAttackerDuringBlockers: false,
    })).toBe("single");
  });

  it("keeps two to four matching creatures staggered", () => {
    for (const count of [2, 3, 4]) {
      expect(getCreatureGroupRenderMode(group(count), "creatures", {
        manualExpanded: false,
        containsCommittedAttackerDuringBlockers: false,
      })).toBe("staggered");
    }
  });

  it("collapses five or more matching creatures", () => {
    expect(getCreatureGroupRenderMode(group(5), "creatures", {
      manualExpanded: false,
      containsCommittedAttackerDuringBlockers: false,
    })).toBe("collapsed");
  });

  it("does not collapse non-creature rows", () => {
    expect(getCreatureGroupRenderMode(group(8), "lands", {
      manualExpanded: false,
      containsCommittedAttackerDuringBlockers: false,
    })).toBe("staggered");
  });

  it("lets manual expansion and committed attackers win over collapsed mode", () => {
    expect(getCreatureGroupRenderMode(group(5), "creatures", {
      manualExpanded: true,
      containsCommittedAttackerDuringBlockers: false,
    })).toBe("expanded");
    expect(getCreatureGroupRenderMode(group(5), "creatures", {
      manualExpanded: false,
      containsCommittedAttackerDuringBlockers: true,
    })).toBe("expanded");
  });

  it("reports sizing slots and stagger counts from the render mode", () => {
    const five = group(5);

    expect(visibleCardSlotCount("collapsed", five)).toBe(1);
    expect(visibleStaggerCount("collapsed", five)).toBe(0);
    expect(visibleCardSlotCount("expanded", five)).toBe(5);
    expect(visibleStaggerCount("expanded", five)).toBe(0);
    expect(visibleCardSlotCount("staggered", five)).toBe(1);
    expect(visibleStaggerCount("staggered", five)).toBe(4);
  });
});
