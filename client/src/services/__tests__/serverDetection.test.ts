import { afterEach, describe, expect, it } from "vitest";

import {
  DEFAULT_MULTIPLAYER_SERVER_URL,
  OFFICIAL_MULTIPLAYER_SERVER_URL,
} from "../../config/multiplayerServer";
import {
  DEFAULT_SERVER,
  SERVER_PRESETS,
  formatJoinShare,
  mixedContentBlockReason,
  parseJoinCode,
} from "../serverDetection";

describe("server defaults", () => {
  it("uses the configured build default as the first server preset", () => {
    expect(DEFAULT_SERVER).toBe(DEFAULT_MULTIPLAYER_SERVER_URL);
    expect(DEFAULT_SERVER).toBe(SERVER_PRESETS[0].url);
    expect(SERVER_PRESETS).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          labelKey: "serverPicker.official",
          url: OFFICIAL_MULTIPLAYER_SERVER_URL,
        }),
      ]),
    );
  });
});

describe("parseJoinCode", () => {
  it("returns just the code when no server address is present", () => {
    expect(parseJoinCode("ABC123")).toEqual({ code: "ABC123" });
  });

  it("defaults a bare remote host to wss on the standard TLS port (443)", () => {
    // A no-port remote host is a TLS proxy/tunnel (ngrok, Cloudflare, Caddy),
    // so the port suffix is omitted rather than defaulting to the raw port.
    expect(parseJoinCode("ABC123@play.example.com")).toEqual({
      code: "ABC123",
      serverAddress: "wss://play.example.com/ws",
    });
  });

  it("respects an explicit ws:// scheme for a plain-ws LAN server", () => {
    expect(parseJoinCode("ABC123@ws://192.168.1.5:9374")).toEqual({
      code: "ABC123",
      serverAddress: "ws://192.168.1.5:9374/ws",
    });
  });

  it("respects an explicit wss:// scheme and drops the default 443 suffix", () => {
    expect(parseJoinCode("ABC123@wss://play.example.com:443")).toEqual({
      code: "ABC123",
      serverAddress: "wss://play.example.com/ws",
    });
  });

  it("defaults a bare remote host with an explicit port to wss on that port", () => {
    expect(parseJoinCode("ABC123@1.2.3.4:9374")).toEqual({
      code: "ABC123",
      serverAddress: "wss://1.2.3.4:9374/ws",
    });
  });

  it("uses ws:// for loopback hosts", () => {
    expect(parseJoinCode("ABC123@localhost:9374")).toEqual({
      code: "ABC123",
      serverAddress: "ws://localhost:9374/ws",
    });
  });

  it("returns just the code when the address is empty after @", () => {
    expect(parseJoinCode("ABC123@")).toEqual({ code: "ABC123" });
  });

  it("resolves an ngrok tunnel host (no port) to wss on 443", () => {
    // The share string emits the bare ngrok host; the joiner must reach it over wss.
    expect(parseJoinCode("ABC123@x.ngrok-free.app")).toEqual({
      code: "ABC123",
      serverAddress: "wss://x.ngrok-free.app/ws",
    });
  });
});

describe("formatJoinShare", () => {
  it("emits host only for an https tunnel/proxy URL", () => {
    expect(formatJoinShare("K42QQS", "https://x.ngrok-free.app")).toBe(
      "K42QQS@x.ngrok-free.app",
    );
  });

  it("preserves ws:// for a plain-ws LAN PUBLIC_URL so the joiner reaches the non-TLS server", () => {
    expect(formatJoinShare("K42QQS", "ws://192.168.1.5:9374")).toBe(
      "K42QQS@ws://192.168.1.5:9374",
    );
  });

  it("drops the path and keeps a non-default port", () => {
    expect(formatJoinShare("K42QQS", "https://play.example.com:8443/ws")).toBe(
      "K42QQS@play.example.com:8443",
    );
  });

  it("returns null on a malformed public URL", () => {
    expect(formatJoinShare("K42QQS", "not a url")).toBeNull();
  });

  it("round-trips: an https share string parses back to the wss connect URL", () => {
    const share = formatJoinShare("K42QQS", "https://x.ngrok-free.app");
    expect(share).not.toBeNull();
    expect(parseJoinCode(share!)).toEqual({
      code: "K42QQS",
      serverAddress: "wss://x.ngrok-free.app/ws",
    });
  });

  it("round-trips: a ws:// LAN share string preserves the plain-ws scheme", () => {
    const share = formatJoinShare("K42QQS", "ws://192.168.1.5:9374");
    expect(share).not.toBeNull();
    expect(parseJoinCode(share!)).toEqual({
      code: "K42QQS",
      serverAddress: "ws://192.168.1.5:9374/ws",
    });
  });
});

describe("mixedContentBlockReason", () => {
  const originalLocation = window.location;

  function setPageProtocol(protocol: "http:" | "https:") {
    Object.defineProperty(window, "location", {
      value: { ...originalLocation, protocol },
      writable: true,
      configurable: true,
    });
  }

  afterEach(() => {
    Object.defineProperty(window, "location", {
      value: originalLocation,
      writable: true,
      configurable: true,
    });
  });

  it("blocks a remote ws:// target from an HTTPS page", () => {
    setPageProtocol("https:");
    const reason = mixedContentBlockReason("ws://70.249.47.161:9374/ws");
    expect(reason).toMatch(/HTTPS/);
    expect(reason).toContain("70.249.47.161:9374");
  });

  it("allows ws:// to loopback even from an HTTPS page", () => {
    setPageProtocol("https:");
    expect(mixedContentBlockReason("ws://localhost:9374/ws")).toBeNull();
  });

  it("allows ws:// from an http:// page", () => {
    setPageProtocol("http:");
    expect(mixedContentBlockReason("ws://70.249.47.161:9374/ws")).toBeNull();
  });

  it("never blocks a wss:// target", () => {
    setPageProtocol("https:");
    expect(mixedContentBlockReason("wss://play.example.com/ws")).toBeNull();
  });
});
