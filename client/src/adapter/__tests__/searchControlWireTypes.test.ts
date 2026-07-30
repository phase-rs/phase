import { describe, expect, it } from "vitest";

import type {
  ActiveLibrarySearches,
  ActiveSearchDecisionControls,
  GameEvent,
  ObjectIncarnationRef,
} from "../types";

describe("search-control wire types", () => {
  it("preserves simultaneous keyed provenance and private event snapshots", () => {
    const searches: ActiveLibrarySearches = {
      "0": {
        searcher: 0,
        searched_zone_owner: 2,
        effective_library_owner: 2,
        learned_audience: [0, 3],
        looked_at: [[2, "Library", { object_id: 21, incarnation: 4 }]],
      },
      "1": {
        searcher: 1,
        searched_zone_owner: 2,
        learned_audience: [1],
        looked_at: [[2, "Hand", { object_id: 22, incarnation: 7 }]],
      },
      "3": {
        searcher: 3,
        searched_zone_owner: 3,
        effective_library_owner: 3,
        learned_audience: [3],
        looked_at: [],
      },
    };
    const controls: ActiveSearchDecisionControls = {
      "0": { searcher: 0, searched_zone_owner: 2, authority: { type: "latched_controller", controller: 3 } },
      "1": { searcher: 1, searched_zone_owner: 2, authority: { type: "searcher_fallback" } },
      "3": { searcher: 3, searched_zone_owner: 3, authority: { type: "latched_controller", controller: 3 } },
    };
    const event: Extract<GameEvent, { type: "HiddenSearchViewed" }> = {
      type: "HiddenSearchViewed",
      data: { searcher: 1, cards: [], audience: [1] },
    };

    expect(JSON.parse(JSON.stringify({ searches, controls, event }))).toEqual({
      searches,
      controls,
      event,
    });
    expect(searches["1"]?.effective_library_owner).toBeUndefined();
    expect(controls["0"]?.searched_zone_owner).toBe(2);
    expect(controls["0"]?.searched_zone_owner).not.toBe(controls["0"]?.searcher);
  });

  it("rejects malformed keys and raw object ids at compile time", () => {
    const identity: ObjectIncarnationRef = { object_id: 9, incarnation: 2 };
    expect(identity.incarnation).toBe(2);

    const searches: ActiveLibrarySearches = {
      // @ts-expect-error serialized PlayerId keys must use numeric syntax
      player_one: {
        searcher: 1,
        searched_zone_owner: 1,
        learned_audience: [1],
        looked_at: [],
      },
    };
    // @ts-expect-error exact search identity cannot be represented by a raw ObjectId
    const rawIdentity: ObjectIncarnationRef = 9;
    expect(searches).toBeDefined();
    expect(rawIdentity).toBeDefined();
  });
});
