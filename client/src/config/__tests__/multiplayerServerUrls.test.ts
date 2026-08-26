import { describe, expect, it } from "vitest";

import {
  PRODUCTION_MULTIPLAYER_SERVER_URL,
  resolveMultiplayerServerUrls,
} from "../multiplayerServerUrls";

const PREVIEW = "wss://lobby-preview.phase-rs.dev/ws";
const SELF_HOSTED = "wss://play.example.com/ws";

/** Build an env reader from a plain map; unset names read as "" like
 * vite.config.ts's own `envVar` does. */
const env = (vars: Record<string, string>) => (name: string) => vars[name] ?? "";

describe("resolveMultiplayerServerUrls", () => {
  it("falls back to production when nothing is set (release build)", () => {
    expect(resolveMultiplayerServerUrls(env({}))).toEqual({
      official: PRODUCTION_MULTIPLAYER_SERVER_URL,
      buildDefault: PRODUCTION_MULTIPLAYER_SERVER_URL,
    });
  });

  // The regression guard. Setting OFFICIAL alone must ALSO move buildDefault:
  // serverDetection reads `buildDefault !== official` as "self-hosted build",
  // so leaving buildDefault on production would prepend a self-hosted preset
  // and make the production lobby SERVER_PRESETS[0] — the default pick — for
  // every preview user, which is the one broker a preview build cannot
  // handshake with.
  it("moves the build default with the official url (preview build)", () => {
    const resolved = resolveMultiplayerServerUrls(
      env({ OFFICIAL_MULTIPLAYER_SERVER_URL: PREVIEW }),
    );
    expect(resolved.official).toBe(PREVIEW);
    expect(resolved.buildDefault).toBe(PREVIEW);
    expect(resolved.buildDefault).not.toBe(PRODUCTION_MULTIPLAYER_SERVER_URL);
  });

  it("keeps the self-hoster seam: DEFAULT alone diverges from official", () => {
    expect(
      resolveMultiplayerServerUrls(env({ DEFAULT_MULTIPLAYER_SERVER_URL: SELF_HOSTED })),
    ).toEqual({
      official: PRODUCTION_MULTIPLAYER_SERVER_URL,
      buildDefault: SELF_HOSTED,
    });
  });

  it("lets an explicit DEFAULT win over the official url when both are set", () => {
    expect(
      resolveMultiplayerServerUrls(
        env({
          OFFICIAL_MULTIPLAYER_SERVER_URL: PREVIEW,
          DEFAULT_MULTIPLAYER_SERVER_URL: SELF_HOSTED,
        }),
      ),
    ).toEqual({ official: PREVIEW, buildDefault: SELF_HOSTED });
  });

  it("treats an empty string as unset", () => {
    expect(
      resolveMultiplayerServerUrls(
        env({
          OFFICIAL_MULTIPLAYER_SERVER_URL: "",
          DEFAULT_MULTIPLAYER_SERVER_URL: "",
        }),
      ),
    ).toEqual({
      official: PRODUCTION_MULTIPLAYER_SERVER_URL,
      buildDefault: PRODUCTION_MULTIPLAYER_SERVER_URL,
    });
  });
});
