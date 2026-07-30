import { describe, expect, it } from "vitest";

import {
  loopDetectionModeFromQuery,
  loopDetectionModeToQuery,
} from "../loopDetectionMode";

describe("loop detection URL mode", () => {
  it("round-trips every selectable loop-detection mode", () => {
    expect(loopDetectionModeToQuery({ type: "Off" })).toBeNull();
    expect(loopDetectionModeToQuery({ type: "On" })).toBe("on");
    expect(loopDetectionModeToQuery({ type: "Interactive" })).toBe("interactive");

    expect(loopDetectionModeFromQuery(null)).toEqual({ type: "Off" });
    expect(loopDetectionModeFromQuery("on")).toEqual({ type: "On" });
    expect(loopDetectionModeFromQuery("INTERACTIVE")).toEqual({ type: "Interactive" });
  });

  it("defaults unknown query values to Off", () => {
    expect(loopDetectionModeFromQuery("unexpected")).toEqual({ type: "Off" });
  });
});
