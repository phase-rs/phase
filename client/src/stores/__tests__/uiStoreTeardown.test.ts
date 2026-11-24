import { afterAll, describe, expect, it, vi } from "vitest";

import { useUiStore } from "../uiStore";

const HOVER_PROBE = "[data-card-hover]:hover";

describe("uiStore hover-clear timer does not outlive its test", () => {
  const querySelector = vi.spyOn(document, "querySelector");
  let armed = false;

  afterAll(() => {
    querySelector.mockRestore();
  });

  it("leaves a deferred preview clear pending at test end", () => {
    useUiStore.getState().inspectObject(7, 0, "immediate");
    expect(useUiStore.getState().inspectedObjectId).toBe(7);
    useUiStore.getState().inspectObject(null);
    armed = true;
  });

  it("finds that clear cancelled by the shared afterEach teardown", async () => {
    expect(armed).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 80));
    expect(querySelector).not.toHaveBeenCalledWith(HOVER_PROBE);
  });
});
