import { describe, expect, it } from "vitest";

import {
  loopDetectionModeFromQuery,
  loopDetectionModeToQuery,
} from "../loopDetectionMode";

describe("loop detection URL mode", () => {
  it("round-trips every selectable loop-detection mode", () => {
    expect(loopDetectionModeToQuery({ type: "Off" })).toBeNull();
    expect(loopDetectionModeToQuery({ type: "Interactive" })).toBe("interactive");

    expect(loopDetectionModeFromQuery(null)).toEqual({ type: "Off" });
    expect(loopDetectionModeFromQuery("INTERACTIVE")).toEqual({ type: "Interactive" });
  });

  it("coerces the retired 'on' query value forward to Interactive, not back to Off", () => {
    expect(loopDetectionModeFromQuery("on")).toEqual({ type: "Interactive" });
  });

  it("defaults unknown query values to Off", () => {
    expect(loopDetectionModeFromQuery("unexpected")).toEqual({ type: "Off" });
  });
});
