import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import capabilities from "../../../src-tauri/capabilities/default.json";

const mocks = vi.hoisted(() => ({
  bundled: false,
  tauri: false,
  moduleLoaded: vi.fn(),
  openUrl: vi.fn(),
}));

vi.mock("../platform", () => ({
  isBundledTauriOrigin: () => mocks.bundled,
  isTauri: () => mocks.tauri,
}));
vi.mock("@tauri-apps/plugin-opener", () => {
  mocks.moduleLoaded();
  return { openUrl: mocks.openUrl };
});

import { FIRST_PARTY_ORIGINS, installTauriExternalLinkHandler } from "../externalLinks";

function click(href: string, nested = false, init: MouseEventInit = {}): MouseEvent {
  const anchor = document.createElement("a");
  anchor.setAttribute("href", href);
  const target = nested ? document.createElement("span") : anchor;
  if (nested) anchor.append(target);
  document.body.append(anchor);
  const event = new MouseEvent("click", { bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  anchor.remove();
  return event;
}

beforeAll(() => {
  mocks.tauri = true;
  installTauriExternalLinkHandler();
});

afterEach(() => {
  mocks.bundled = false;
  mocks.tauri = true;
  mocks.openUrl.mockReset();
  vi.restoreAllMocks();
});

describe("Tauri document external-link routing", () => {
  it("keeps first-party origins aligned with the remote capability manifest", () => {
    const manifestOrigins = capabilities
      .flatMap((capability) =>
        "remote" in capability && capability.remote ? capability.remote.urls : [],
      )
      .map((url) => new URL(url.replace(/\*$/, "")).origin);

    expect(new Set(manifestOrigins)).toEqual(FIRST_PARTY_ORIGINS);
  });

  it.each([
    "https://",
    "//example.com/cards",
    "\\\\example.com/cards",
    "file:///tmp/card.txt",
    "mailto:player@example.com",
    "tel:+123456",
    "phase://deck/1",
  ])("blocks invalid or non-HTTP(S) anchor %s without loading opener", (url) => {
    expect(click(url).defaultPrevented).toBe(true);
    expect(mocks.moduleLoaded).not.toHaveBeenCalled();
    expect(mocks.openUrl).not.toHaveBeenCalled();
  });

  it.each(["/setup", "my-decks", "?tab=recent", "#news"])(
    "preserves React Router-style relative menu link %s",
    (url) => {
      expect(click(url).defaultPrevented).toBe(false);
      expect(mocks.moduleLoaded).not.toHaveBeenCalled();
      expect(mocks.openUrl).not.toHaveBeenCalled();
    },
  );

  it("ignores an anchor without an href", () => {
    const anchor = document.createElement("a");
    document.body.append(anchor);
    const event = new MouseEvent("click", { bubbles: true, cancelable: true });
    anchor.dispatchEvent(event);
    anchor.remove();
    expect(event.defaultPrevented).toBe(false);
    expect(mocks.moduleLoaded).not.toHaveBeenCalled();
    expect(mocks.openUrl).not.toHaveBeenCalled();
  });

  it("routes a nested bundled HTTPS anchor and modifier click through opener", async () => {
    mocks.bundled = true;
    const event = click("https://example.com/cards", true, { ctrlKey: true });
    expect(event.defaultPrevented).toBe(true);
    await vi.waitFor(() => expect(mocks.openUrl).toHaveBeenCalledWith("https://example.com/cards"));
    expect(mocks.moduleLoaded).toHaveBeenCalledOnce();
  });

  it("normalizes a whitespace-prefixed external URL before routing it through opener", async () => {
    const event = click(" https://evil.example/x");
    expect(event.defaultPrevented).toBe(true);
    await vi.waitFor(() => expect(mocks.openUrl).toHaveBeenCalledWith("https://evil.example/x"));
  });

  it.each([
    "https://phase-rs.dev/play",
    "https://app.phase-rs.dev/play",
    "https://preview.phase-rs.dev/play",
  ])("preserves first-party remote-shell navigation for %s", (url) => {
    expect(click(url).defaultPrevented).toBe(false);
    expect(mocks.openUrl).not.toHaveBeenCalled();
  });

  it("installs only once", () => {
    const add = vi.spyOn(document, "addEventListener");
    installTauriExternalLinkHandler();
    expect(add).not.toHaveBeenCalled();
  });

  it("does not install in a browser from a fresh module registry", async () => {
    vi.resetModules();
    mocks.tauri = false;
    const add = vi.spyOn(document, "addEventListener");
    const freshModule = await import("../externalLinks");

    freshModule.installTauriExternalLinkHandler();

    expect(add).not.toHaveBeenCalled();
  });

  it("ignores an event already handled by another capture listener", () => {
    const anchor = document.createElement("a");
    anchor.href = "https://example.com";
    document.body.append(anchor);
    const event = new MouseEvent("click", { bubbles: true, cancelable: true });
    event.preventDefault();
    anchor.dispatchEvent(event);
    anchor.remove();
    expect(mocks.openUrl).not.toHaveBeenCalled();
  });

  it("contains opener rejection", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    mocks.openUrl.mockRejectedValueOnce(new Error("denied"));
    expect(click("http://example.com").defaultPrevented).toBe(true);
    await vi.waitFor(() => expect(warn).toHaveBeenCalledOnce());
  });
});
