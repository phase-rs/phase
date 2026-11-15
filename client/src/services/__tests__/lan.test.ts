import { beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock, desktop, offline } = vi.hoisted(() => ({
  invokeMock: vi.fn(), desktop: vi.fn(), offline: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("../platform", () => ({ isDesktopTauri: desktop }));
vi.mock("../../stores/connectivityStore", () => ({ getEffectiveOffline: offline }));

beforeEach(() => {
  vi.resetModules(); vi.clearAllMocks();
  desktop.mockReturnValue(true); offline.mockReturnValue(false);
  vi.stubGlobal("window", { location: { origin: "https://phase-rs.dev" } });
  invokeMock.mockResolvedValue({ supported: true });
});

describe("LAN capability and lifecycle", () => {
  it.each(["browser", "Android", "iOS"])("never invokes native commands on %s", async () => {
    desktop.mockReturnValue(false);
    const lan = await import("../lan");
    expect(await lan.initializeLanCapabilities()).toBe(false);
    await expect(lan.stopLanServer()).rejects.toThrow(/unavailable/);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("probes an old shell once and rejects mutations", async () => {
    invokeMock.mockRejectedValue(new Error("unknown command"));
    const lan = await import("../lan");
    expect(await lan.initializeLanCapabilities()).toBe(false);
    await expect(lan.startLanServer()).rejects.toThrow(/unavailable/);
    expect(invokeMock.mock.calls).toEqual([["lan_capabilities"]]);
    expect(lan.canUseLanBridge("ws://192.168.1.2:9374/ws")).toBe(false);
  });

  it.each([false, true])("starts with current key and offline=%s policy", async (isOffline) => {
    offline.mockReturnValue(isOffline);
    const lan = await import("../lan");
    await lan.startLanServer();
    expect(invokeMock).toHaveBeenLastCalledWith("start_lan_server", {
      key: { release: { version: __APP_VERSION__ } },
      intent: isOffline ? "start_offline" : "start_online",
    });
    await lan.getLanServerStatus();
    expect(invokeMock).toHaveBeenLastCalledWith("lan_server_status", undefined);
    await lan.stopLanServer();
    expect(invokeMock).toHaveBeenLastCalledWith("stop_lan_server", undefined);
  });

  it("requires a completed probe and supported origin before routing", async () => {
    const lan = await import("../lan");
    const url = "ws://10.0.0.2:9374/ws";
    expect(lan.canUseLanBridge(url)).toBe(false);
    await lan.initializeLanCapabilities();
    expect(lan.canUseLanBridge(url)).toBe(true);
    vi.stubGlobal("window", { location: { origin: "https://example.com" } });
    expect(lan.canUseLanBridge(url)).toBe(false);
  });

  it("deduplicates discovery and allows retry after an error", async () => {
    const lan = await import("../lan");
    await lan.initializeLanCapabilities();
    invokeMock.mockRejectedValueOnce({ detail: "multicast unavailable" });
    await expect(lan.discoverLanServers()).rejects.toEqual({ detail: "multicast unavailable" });
    const server = { name: "Local", url: "ws://10.0.0.2:9374/ws", channel: null };
    invokeMock.mockResolvedValueOnce([server, server, { ...server, url: "wss://example.com/ws" }]);
    expect(await lan.discoverLanServers()).toEqual([server]);
  });
});

describe("native endpoint grammar", () => {
  it("routes a stored URL after default-port normalization", async () => {
    const { parseWebSocketUrl } = await import("../../config/multiplayerServer");
    const lan = await import("../lan");
    const storedUrl = parseWebSocketUrl("ws://192.168.1.2:80/ws")!.href;
    expect(storedUrl).toBe("ws://192.168.1.2/ws");
    await lan.initializeLanCapabilities();
    expect(lan.canUseLanBridge(storedUrl)).toBe(true);
    expect(lan.normalizeLanEndpoint(storedUrl)).toBe("ws://192.168.1.2:80/ws");
  });

  it.each(["ws://10.0.0.1:1/ws", "ws://172.16.0.1:9374/ws", "ws://172.31.255.255:65535/ws", "ws://192.168.1.4:80/ws", "ws://127.0.0.2:9374/ws"])("accepts %s", async (url) => {
    const { isLanEndpoint } = await import("../lan");
    expect(isLanEndpoint(url)).toBe(true);
  });
  it.each(["ws://8.8.8.8:9374/ws", "ws://0.0.0.0:9374/ws", "ws://224.0.0.1:9374/ws", "ws://172.32.0.1:9374/ws", "ws://192.168.01.2:9374/ws", "ws://127.1:9374/ws", "ws://localhost:9374/ws", "ws://10.0.0.1:0/ws", "ws://10.0.0.1:65536/ws", "ws://10.0.0.1:9374/other", "ws://10.0.0.1:9374/ws?q=x", "ws://user@10.0.0.1:9374/ws", "wss://10.0.0.1:9374/ws"])("refuses %s", async (url) => {
    const { isLanEndpoint } = await import("../lan");
    expect(isLanEndpoint(url)).toBe(false);
  });
});
