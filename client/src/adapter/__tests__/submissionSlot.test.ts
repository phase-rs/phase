import { describe, expect, it, vi } from "vitest";

import { SubmissionSlot } from "../submissionSlot";

const claim = (slot: SubmissionSlot, correlate: boolean) => slot.claim(vi.fn(), vi.fn(), correlate);

describe("SubmissionSlot", () => {
  it("stamps monotonically increasing ids only when correlating", () => {
    const slot = new SubmissionSlot();
    const a = claim(slot, true);
    expect(slot.stamp({ type: "Action" }, a)).toEqual({ type: "Action", command_id: a.commandId });
    slot.drop();
    const b = claim(slot, true);
    expect(b.commandId).toBe((a.commandId as number) + 1);
    slot.drop();
    const legacy = claim(slot, false);
    expect(legacy.commandId).toBeNull();
    expect(slot.stamp({ type: "Action" }, legacy)).toEqual({ type: "Action" });
  });

  it("settles a stamped submission only on its own id", () => {
    const slot = new SubmissionSlot();
    const sub = claim(slot, true);
    expect(slot.take(undefined)).toBeNull();
    expect(slot.take((sub.commandId as number) + 1)).toBeNull();
    expect(slot.occupied).toBe(true);
    expect(slot.take(sub.commandId)).toBe(sub);
    expect(slot.occupied).toBe(false);
  });

  it("settles an unstamped submission on any completion frame", () => {
    const slot = new SubmissionSlot();
    const sub = claim(slot, false);
    expect(slot.take(99)).toBe(sub);
    expect(slot.take(undefined)).toBeNull();
  });

  it("decides correlation from the pending submission, not from how the next one is stamped", () => {
    const slot = new SubmissionSlot();
    claim(slot, true);
    expect(slot.unmatched(undefined)).toEqual({ releasesIndicator: false, surfaces: true });
    expect(slot.unmatched(7)).toEqual({ releasesIndicator: false, surfaces: false });
    slot.drop();
    expect(slot.unmatched(undefined)).toEqual({ releasesIndicator: true, surfaces: true });
  });

  it("release only empties the slot for its current owner", () => {
    const slot = new SubmissionSlot();
    const first = claim(slot, true);
    slot.drop();
    const second = claim(slot, true);
    expect(slot.release(first)).toBe(false);
    expect(slot.owns(second)).toBe(true);
    expect(slot.release(second)).toBe(true);
  });
});
