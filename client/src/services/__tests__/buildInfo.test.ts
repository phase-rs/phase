import { afterEach, expect, it, vi } from "vitest";

import { displayedBuildHash } from "../buildInfo";

afterEach(() => vi.unstubAllGlobals());

it("hides the build hash outside the preview deployment", () => {
  vi.stubGlobal("__IS_PREVIEW_BUILD__", false);
  expect(displayedBuildHash()).toBeNull();
});

it("shows the build hash on the preview deployment", () => {
  vi.stubGlobal("__IS_PREVIEW_BUILD__", true);
  expect(displayedBuildHash()).toBe(__BUILD_HASH__);
});
