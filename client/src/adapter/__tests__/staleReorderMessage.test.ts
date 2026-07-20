import { describe, expect, it } from "vitest";

import { isStaleActionMessage, isStaleReorderMessage } from "../types.ts";

describe("isStaleReorderMessage (issue #5913)", () => {
  it("matches the engine's ReorderHand count mismatch, whatever the counts", () => {
    // Verbatim shape from `apply_action`:
    //   EngineError::InvalidAction(format!("ReorderHand: expected {} ids, got {}", ..))
    // surfaced by the wasm bridge as `Engine error: <display>`.
    expect(isStaleReorderMessage("Engine error: ReorderHand: expected 6 ids, got 5")).toBe(true);
    expect(isStaleReorderMessage("Engine error: ReorderHand: expected 1 ids, got 0")).toBe(true);
    expect(isStaleReorderMessage("Engine error: ReorderHand: expected 12 ids, got 13")).toBe(true);
  });

  it("matches the same-length staleness rejection too", () => {
    // A discard AND a draw inside one animation window leaves the count equal
    // but the ids changed, so the engine reaches its permutation check instead
    // of the count check. Same benign race, verbatim message from
    // `apply_action`.
    expect(
      isStaleReorderMessage("Engine error: ReorderHand: order is not a permutation of the current hand"),
    ).toBe(true);
  });

  it("does not swallow the invalid-actor rejection", () => {
    // That one means the caller submitted a nonsense seat — a real bug, and it
    // must keep surfacing rather than being silently dropped.
    expect(
      isStaleReorderMessage("Engine error: ReorderHand: actor PlayerId(3) is not a valid player index"),
    ).toBe(false);
  });

  it("does not match unrelated engine errors", () => {
    expect(isStaleReorderMessage("Engine error: Wrong player")).toBe(false);
    expect(isStaleReorderMessage("Engine error: Not your priority")).toBe(false);
    expect(isStaleReorderMessage("NOT_INITIALIZED: no game")).toBe(false);
    expect(isStaleReorderMessage("")).toBe(false);
  });

  it("is disjoint from the actor-authorization predicate", () => {
    // The two matchers classify to the same benign code but must not overlap,
    // so each keeps its own documented meaning.
    const reorder = "Engine error: ReorderHand: expected 6 ids, got 5";
    const authz = "Engine error: Wrong player";
    expect(isStaleReorderMessage(reorder) && isStaleActionMessage(reorder)).toBe(false);
    expect(isStaleActionMessage(authz) && isStaleReorderMessage(authz)).toBe(false);
  });
});
