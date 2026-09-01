import { afterEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  tauri: false,
  moduleLoaded: vi.fn(),
  openUrl: vi.fn(),
}));

vi.mock("../platform", () => ({ isTauri: () => mocks.tauri }));
vi.mock("@tauri-apps/plugin-opener", () => {
  mocks.moduleLoaded();
  return { openUrl: mocks.openUrl };
});

import { openExternal } from "../openExternal";

const trustedEvent = { isTrusted: true } as Event;

afterEach(() => {
  mocks.tauri = false;
  mocks.openUrl.mockReset();
  vi.restoreAllMocks();
});

describe("direct external-link routing", () => {
  it.each([
    "https://",
    "/relative",
    "file:///tmp/card.txt",
    "mailto:player@example.com",
    "tel:+123456",
    "phase://deck/1",
  ])("rejects %s before either browser route is reached", (url) => {
    const browserOpen = vi.spyOn(window, "open").mockImplementation(() => null);
    openExternal(url, trustedEvent);
    expect(mocks.moduleLoaded).not.toHaveBeenCalled();
    expect(mocks.openUrl).not.toHaveBeenCalled();
    expect(browserOpen).not.toHaveBeenCalled();
  });

  it("uses the exact browser window.open contract for HTTPS", () => {
    const browserOpen = vi.spyOn(window, "open").mockImplementation(() => null);
    openExternal("https://example.com/cards", trustedEvent);
    expect(browserOpen).toHaveBeenCalledWith(
      "https://example.com/cards",
      "_blank",
      "noopener,noreferrer",
    );
    expect(mocks.moduleLoaded).not.toHaveBeenCalled();
  });

  it("dynamically routes Tauri HTTP URLs through opener", async () => {
    mocks.tauri = true;
    const browserOpen = vi.spyOn(window, "open").mockImplementation(() => null);
    openExternal("http://example.com", trustedEvent);
    await vi.waitFor(() => expect(mocks.openUrl).toHaveBeenCalledWith("http://example.com"));
    expect(browserOpen).not.toHaveBeenCalled();
  });

  it("contains opener import or invocation rejection", async () => {
    mocks.tauri = true;
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    mocks.openUrl.mockRejectedValueOnce(new Error("denied"));
    openExternal("https://example.com", trustedEvent);
    await vi.waitFor(() => expect(warn).toHaveBeenCalledOnce());
  });

  it("refuses an untrusted event before opening a browser window", () => {
    const browserOpen = vi.spyOn(window, "open").mockImplementation(() => null);

    openExternal("https://example.com/cards", new Event("click"));

    expect(browserOpen).not.toHaveBeenCalled();
    expect(mocks.moduleLoaded).not.toHaveBeenCalled();
  });
});
