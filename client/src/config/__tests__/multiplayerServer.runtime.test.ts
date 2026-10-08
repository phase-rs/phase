import { afterEach, describe, expect, it, vi } from "vitest";

import { parseWebSocketUrl } from "../multiplayerServer";

// DEFAULT_MULTIPLAYER_SERVER_URL is resolved once at module load, so every case
// sets window.__PHASE_CONFIG__ first and then imports a fresh copy.
async function loadWith(config: unknown) {
  vi.resetModules();
  if (config === undefined) {
    delete (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__;
  } else {
    (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__ = config;
  }
  return import("../multiplayerServer");
}

const SELF_HOSTED = "wss://play.example.com/ws";

afterEach(() => {
  delete (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__;
  vi.resetModules();
});

describe("DEFAULT_MULTIPLAYER_SERVER_URL runtime override", () => {
  it("uses the build-time define when no runtime config is present", async () => {
    const mod = await loadWith(undefined);
    expect(mod.DEFAULT_MULTIPLAYER_SERVER_URL).toBe(__DEFAULT_MULTIPLAYER_SERVER_URL__);
  });

  it("uses the build-time define when the deployment shipped an empty config", async () => {
    const mod = await loadWith({});
    expect(mod.DEFAULT_MULTIPLAYER_SERVER_URL).toBe(__DEFAULT_MULTIPLAYER_SERVER_URL__);
  });

  it("prefers a valid runtime override over the build-time define", async () => {
    // Guards the assertion below from passing vacuously: if the fixture ever
    // equalled the define, "override wins" would be indistinguishable from
    // "override ignored".
    expect(SELF_HOSTED).not.toBe(__DEFAULT_MULTIPLAYER_SERVER_URL__);

    const mod = await loadWith({ multiplayerServerUrl: SELF_HOSTED });
    expect(mod.DEFAULT_MULTIPLAYER_SERVER_URL).toBe(SELF_HOSTED);
  });

  it("accepts ws:// as well as wss:// (a LAN deployment has no TLS)", async () => {
    const mod = await loadWith({ multiplayerServerUrl: "ws://192.168.1.5:9374/ws" });
    expect(mod.DEFAULT_MULTIPLAYER_SERVER_URL).toBe("ws://192.168.1.5:9374/ws");
  });

  // A typo'd address would otherwise be handed to every new profile as its
  // default, with nothing to tell the player why nothing connects.
  it.each([
    ["a non-websocket scheme", "https://play.example.com"],
    ["a bare hostname", "play.example.com"],
    ["an empty string", ""],
    ["a scheme with no host", "wss://"],
    ["a fragment the WebSocket constructor rejects", "wss://play.example.com/ws#lobby"],
    ["a non-string", 1234],
    ["null", null],
  ])("ignores %s and falls back to the define", async (_label, value) => {
    const mod = await loadWith({ multiplayerServerUrl: value });
    expect(mod.DEFAULT_MULTIPLAYER_SERVER_URL).toBe(__DEFAULT_MULTIPLAYER_SERVER_URL__);
  });

  it("ignores a config that is not an object at all", async () => {
    const mod = await loadWith("not-a-config");
    expect(mod.DEFAULT_MULTIPLAYER_SERVER_URL).toBe(__DEFAULT_MULTIPLAYER_SERVER_URL__);
  });
});

describe("runtime override reaches the server picker", () => {
  // The user-visible consequence, and the reason the override targets DEFAULT
  // rather than OFFICIAL: serverDetection reads DEFAULT !== OFFICIAL as
  // "self-hosted build", prepends that preset, and SERVER_PRESETS[0] becomes
  // the default pick.
  it("makes the configured server the default pick and adds a self-hosted preset", async () => {
    vi.resetModules();
    (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__ = {
      multiplayerServerUrl: SELF_HOSTED,
    };
    const detection = await import("../../services/serverDetection");

    expect(detection.DEFAULT_SERVER).toBe(SELF_HOSTED);
    expect(detection.SERVER_PRESETS[0]).toEqual({
      labelKey: "serverPicker.selfHosted",
      url: SELF_HOSTED,
    });
    // The official entry survives, so a self-hoster's players can still reach it.
    expect(detection.SERVER_PRESETS.some((p) => p.labelKey === "serverPicker.official")).toBe(true);
  });

  it("leaves the official build with a single preset and no self-hosted row", async () => {
    vi.resetModules();
    delete (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__;
    const detection = await import("../../services/serverDetection");

    expect(detection.SERVER_PRESETS.every((p) => p.labelKey !== "serverPicker.selfHosted")).toBe(
      true,
    );
    expect(detection.DEFAULT_SERVER).toBe(__DEFAULT_MULTIPLAYER_SERVER_URL__);
  });
});

describe("parseWebSocketUrl", () => {
  it.each([
    ["wss://play.example.com/ws"],
    ["ws://192.168.1.5:9374/ws"],
    ["wss://play.example.com/ws?region=eu"],
    // "#" outside a fragment survives as %23, so the guard must not reject it.
    ["wss://play.example.com/ws%23one"],
  ])("accepts %s", (value) => {
    expect(parseWebSocketUrl(value)?.href).toBeTruthy();
  });

  // new WebSocket() throws a SyntaxError on any fragment, so these are not
  // addresses a caller can open — including the bare "#", whose url.hash is ""
  // and which a hash-based guard would wave through.
  it.each([
    ["a named fragment", "wss://play.example.com/ws#lobby"],
    ["a bare trailing hash", "wss://play.example.com/ws#"],
    ["a fragment that looks like a path", "wss://play.example.com/ws#/room/1"],
  ])("rejects %s", (_label, value) => {
    expect(parseWebSocketUrl(value)).toBeNull();
  });
});
