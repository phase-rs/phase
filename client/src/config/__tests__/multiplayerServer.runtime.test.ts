import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

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

// The Helm chart refuses a default-server address the client would silently
// discard. That only holds while the chart's grammar admits no more than
// parseWebSocketUrl does, and the two live in different languages, so the
// regex is read out of the template here rather than copied: a change to one
// without the other fails this test instead of drifting quietly.
describe("chart default-server grammar vs parseWebSocketUrl", () => {
  // Walk up from the working directory so this resolves whether the suite runs
  // from client/ or from the repo root; throws rather than silently skipping.
  const helpers = (() => {
    const rel = "deploy/helm/phase-server/templates/_helpers.tpl";
    for (let dir = process.cwd(); ; dir = dirname(dir)) {
      const candidate = resolve(dir, rel);
      if (existsSync(candidate)) return candidate;
      if (dirname(dir) === dir) throw new Error(`could not locate ${rel} from ${process.cwd()}`);
    }
  })();

  function chartRegex(): RegExp {
    const src = readFileSync(helpers, "utf8");
    const m = src.match(/\{\{- \$re := `([^`]+)` -\}\}/);
    if (!m) throw new Error(`no $re raw string found in ${helpers}`);
    return new RegExp(m[1]);
  }

  const chartAccepts = (v: string) => chartRegex().test(v);

  // Generated, not hand-listed. The first version of this guard listed sample
  // addresses and missed a whole class — bracketed hosts with two elisions, and
  // ones with too few groups and no elision — because nobody thought to write
  // them down. Enumerating the shape instead of the examples is what makes the
  // subset claim mean something.
  const bracketed = new Set<string>();
  for (let n = 1; n <= 10; n++) bracketed.add(Array(n).fill("1").join(":"));
  for (let a = 0; a <= 4; a++)
    for (let b = 0; b <= 4; b++)
      bracketed.add(`${Array(a).fill("1").join(":")}::${Array(b).fill("2").join(":")}`);
  for (let a = 0; a <= 3; a++)
    for (let b = 0; b <= 3; b++)
      for (let c = 0; c <= 3; c++)
        bracketed.add(
          `${Array(a).fill("1").join(":")}::${Array(b).fill("2").join(":")}::${Array(c).fill("3").join(":")}`,
        );
  for (const h of [
    "::1", "::", "::ffff:192.168.1.1", "1:2:3:4:5:6:7:8", "2001:db8::8a2e:370:7334",
    "gggg::1", "1:2:3:4:5:6:7:8:9", "12345::1", "1::2::3", "::ffff:999.1.1.1", "x",
  ]) bracketed.add(h);

  // Dotted-numeric hosts are the second generated class. URL parsing decides a
  // host is an IPv4 attempt from its final label, so these are not hostnames
  // that happen to contain digits — they are addresses that fail to parse.
  const numeric = new Set<string>();
  const MAGS = ["0", "1", "99", "127", "192", "255", "256", "999", "1000", "65535",
                "4294967295", "4294967296", "01", "0x7f", "0xff", "00"];
  for (const n of [1, 2, 3, 4, 5])
    for (const m of MAGS) numeric.add(Array(n).fill(m).join("."));
  for (const h of ["192.168.1.5", "255.255.255.255", "0.0.0.0", "1.2.3", "127.1",
                   "2130706433", "1.2.3.4.5", "999.999.999.999", "256.1.1.1",
                   "0x7f.0.0.1", "example.com", "sub.example.com", "localhost",
                   "host-1.example.com", "xn--bcher-kva.example"]) numeric.add(h);

  const CORPUS = [
    ...[...numeric].map((h) => `wss://${h}/ws`),
    ...[...bracketed].map((h) => `wss://[${h}]/ws`),
    "wss://play.example.com/ws",
    "ws://192.168.1.5:9374/ws",
    "wss://play.example.com/ws?region=eu",
    "wss://play.example.com:65535/ws",
    "wss://play.example.com:0/ws",
    "wss://play.example.com:abc/ws",
    "wss://play.example.com:99999/ws",
    "wss://play.example.com:-1/ws",
    "wss://[::1/ws",
    "wss://[]/ws",
    "wss://]::1[/ws",
    "wss://:9374/ws",
    "wss://@/ws",
    "wss://%00.com/ws",
    "wss://play.example.com bad",
    "wss://play.example.com\tbad",
    "wss://play.example.com/ws#lobby",
    "wss://play.example.com/ws#",
    "https://play.example.com",
    "play.example.com",
    "wss://",
  ];

  it("never admits an address the client would discard", () => {
    const admitted = CORPUS.filter((v) => chartAccepts(v) && parseWebSocketUrl(v) === null);
    expect(admitted).toEqual([]);
  });

  // Without this the case above passes for a chart regex that accepts nothing.
  it("still admits the ordinary addresses operators configure", () => {
    for (const v of ["wss://play.example.com/ws", "ws://192.168.1.5:9374/ws", "wss://[::1]:9374/ws"]) {
      expect(chartAccepts(v), v).toBe(true);
      expect(parseWebSocketUrl(v), v).not.toBeNull();
    }
  });
});
