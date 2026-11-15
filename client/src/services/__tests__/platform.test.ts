import { afterEach, describe, expect, it, vi } from "vitest";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

const originalLocation = window.location;
const originalUserAgent = navigator.userAgent;
const originalMaxTouchPoints = navigator.maxTouchPoints;

function setTauri(enabled: boolean): void {
  if (enabled) {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
  } else {
    Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  }
}

function setUserAgent(userAgent: string, maxTouchPoints = 0): void {
  Object.defineProperty(navigator, "userAgent", { configurable: true, value: userAgent });
  Object.defineProperty(navigator, "maxTouchPoints", {
    configurable: true,
    value: maxTouchPoints,
  });
}

async function loadPlatform() {
  vi.resetModules();
  return import("../platform");
}

afterEach(() => {
  vi.clearAllMocks();
  setTauri(false);
  setUserAgent(originalUserAgent, originalMaxTouchPoints);
  Object.defineProperty(window, "location", {
    configurable: true,
    value: originalLocation,
    writable: true,
  });
});

describe("host platform latch", () => {
  it("resolves plain web without importing or invoking Tauri", async () => {
    setTauri(false);
    const platform = await loadPlatform();
    expect(await platform.initializeHostPlatform()).toBeNull();
    expect(platform.isDesktopTauri()).toBe(false);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("shares one invocation and remains false until the result settles", async () => {
    setTauri(true);
    let resolve!: (value: unknown) => void;
    invokeMock.mockReturnValue(new Promise((done) => { resolve = done; }));
    const platform = await loadPlatform();
    const first = platform.initializeHostPlatform();
    const second = platform.initializeHostPlatform();
    expect(first).toBe(second);
    expect(platform.isDesktopTauri()).toBe(false);
    resolve("desktop");
    await first;
    expect(invokeMock).toHaveBeenCalledOnce();
    expect(platform.isDesktopTauri()).toBe(true);
  });

  it.each(["android", "ios"] as const)("decodes %s and rejects desktop predicates", async (value) => {
    setTauri(true);
    invokeMock.mockResolvedValue(value);
    const platform = await loadPlatform();
    expect(await platform.initializeHostPlatform()).toBe(value);
    expect(platform.isDesktopTauri()).toBe(false);
    expect(value === "android" ? platform.isAndroidTauri() : platform.isIosTauri()).toBe(true);
  });

  it.each([undefined, null, "windows", {}, 7])("fails closed on malformed success %j", async (value) => {
    setTauri(true);
    invokeMock.mockResolvedValue(value);
    const platform = await loadPlatform();
    expect(await platform.initializeHostPlatform()).toBeNull();
    expect(platform.isDesktopTauri()).toBe(false);
  });

  it.each([
    ["Windows", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)"],
    [
      "macOS",
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15",
    ],
    ["Linux", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36"],
  ])("uses proven %s desktop UA evidence after any legacy probe rejection", async (_os, userAgent) => {
    setTauri(true);
    setUserAgent(userAgent);
    invokeMock.mockRejectedValue("Command host_platform not allowed by ACL");
    const platform = await loadPlatform();
    expect(await platform.initializeHostPlatform()).toBe("desktop");
    expect(platform.isDesktopTauri()).toBe(true);
  });

  it.each([
    ["Command host_platform not found", "Mozilla/5.0 (Linux; Android 15)", 0],
    ["Command host_platform not found", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15)", 5],
  ])("fails closed for error/UA ambiguity", async (error, userAgent, touchPoints) => {
    setTauri(true);
    setUserAgent(userAgent, touchPoints);
    invokeMock.mockRejectedValue(error);
    const platform = await loadPlatform();
    expect(await platform.initializeHostPlatform()).toBeNull();
    expect(platform.isDesktopTauri()).toBe(false);
  });
});

describe("isBundledTauriOrigin", () => {
  it.each([
    ["https:", "phase-rs.dev", false],
    ["tauri:", "localhost", true],
    ["http:", "tauri.localhost", true],
  ])("classifies %s//%s", async (protocol, hostname, expected) => {
    Object.defineProperty(window, "location", {
      configurable: true,
      value: { ...originalLocation, protocol, hostname },
      writable: true,
    });
    const platform = await loadPlatform();
    expect(platform.isBundledTauriOrigin()).toBe(expected);
  });
});
