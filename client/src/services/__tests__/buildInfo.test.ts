import { afterEach, expect, it, vi } from "vitest";

import { displayedBuildHash } from "../buildInfo";

const originalLocation = window.location;

function setOrigin(origin: string): void {
  Object.defineProperty(window, "location", {
    configurable: true,
    value: { ...originalLocation, origin },
    writable: true,
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  Object.defineProperty(window, "location", {
    configurable: true,
    value: originalLocation,
    writable: true,
  });
});

it.each([
  { previewBuild: false, origin: "https://preview.phase-rs.dev", expected: null },
  { previewBuild: true, origin: "https://phase-rs.dev", expected: null },
  { previewBuild: true, origin: "http://localhost:3000", expected: null },
  { previewBuild: true, origin: "https://preview.phase-rs.dev", expected: __BUILD_HASH__ },
])(
  "shows the build hash only on the preview build and preview origin (previewBuild=$previewBuild, origin=$origin)",
  ({ previewBuild, origin, expected }) => {
    vi.stubGlobal("__IS_PREVIEW_BUILD__", previewBuild);
    setOrigin(origin);
    expect(displayedBuildHash()).toBe(expected);
  },
);
