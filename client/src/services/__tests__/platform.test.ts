import { afterEach, describe, expect, it } from "vitest";

import { isBundledTauriOrigin } from "../platform";

const originalLocation = window.location;

function setLocation(protocol: string, hostname: string): void {
  Object.defineProperty(window, "location", {
    configurable: true,
    value: { ...originalLocation, protocol, hostname },
    writable: true,
  });
}

afterEach(() => {
  Object.defineProperty(window, "location", {
    configurable: true,
    value: originalLocation,
    writable: true,
  });
});

describe("isBundledTauriOrigin", () => {
  it("returns false for normal web origins", () => {
    setLocation("https:", "phase-rs.dev");

    expect(isBundledTauriOrigin()).toBe(false);
  });

  it("recognizes the tauri custom scheme", () => {
    setLocation("tauri:", "localhost");

    expect(isBundledTauriOrigin()).toBe(true);
  });

  it("recognizes tauri.localhost", () => {
    setLocation("http:", "tauri.localhost");

    expect(isBundledTauriOrigin()).toBe(true);
  });
});
