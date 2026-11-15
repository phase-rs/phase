import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DeckCompatibilityResult } from "../../services/deckCompatibility";
import {
  __resetBotLinkStashForTests,
  BOT_LINK_STASH_MAX_AGE_MS,
  classifyCompatResult,
  clearStashedBotLink,
  hasContinuedOnStaleBuild,
  hostLinkSearch,
  markContinuedOnStaleBuild,
  parseBotLink,
  readStashedBotLinkOnce,
  stashBotLink,
  type HostSeed,
} from "../multiplayerPageState";

function makeResult(
  overrides: Partial<DeckCompatibilityResult> = {},
): DeckCompatibilityResult {
  return {
    standard: { compatible: true, reasons: [] },
    commander: { compatible: true, reasons: [] },
    bo3_ready: true,
    unknown_cards: [],
    selected_format_reasons: [],
    color_identity: [],
    color_distribution: [],
    ...overrides,
  };
}

describe("classifyCompatResult", () => {
  it("returns legal when the engine confirms compatibility", () => {
    const out = classifyCompatResult(
      "Standard",
      makeResult({ selected_format_compatible: true }),
    );
    expect(out).toEqual({ status: "legal", format: "Standard" });
  });

  it("returns illegal with engine-provided reasons when false", () => {
    const out = classifyCompatResult(
      "Commander",
      makeResult({
        selected_format_compatible: false,
        selected_format_reasons: ["Missing commander", "Deck size below 100"],
      }),
    );
    expect(out).toEqual({
      status: "illegal",
      format: "Commander",
      reasons: ["Missing commander", "Deck size below 100"],
    });
  });

  // The key regression guard: an indeterminate engine response (null or
  // undefined) must NOT be treated as "legal". A false-positive green chip
  // would mislead the user into thinking their deck was validated when the
  // engine explicitly declined to make a judgment.
  it("returns idle when the engine can't determine legality (null)", () => {
    const out = classifyCompatResult(
      "Pioneer",
      makeResult({ selected_format_compatible: null }),
    );
    expect(out).toEqual({ status: "idle" });
  });

  it("returns idle when selected_format_compatible is omitted", () => {
    const out = classifyCompatResult("Pauper", makeResult());
    expect(out).toEqual({ status: "idle" });
  });
});

describe("parseBotLink", () => {
  it.each(["", "?view=host-setup", "?format=Commander&players=4"])(
    "ignores a search with no bot-link params: %j",
    (search) => {
      expect(parseBotLink(search)).toBeNull();
    },
  );

  it("reads a full host link", () => {
    expect(
      parseBotLink("?code=AB12CD&format=Commander&players=4&room=%20Friday%20night%20"),
    ).toEqual({
      kind: "host",
      seed: {
        code: "AB12CD",
        format: "Commander",
        playerCount: 4,
        roomName: "Friday night",
        serverUrl: null,
      },
    });
  });

  it("reads a host link with only a code", () => {
    expect(parseBotLink("?code=AB12CD&room=%20%20")).toEqual({
      kind: "host",
      seed: { code: "AB12CD", format: null, playerCount: null, roomName: null, serverUrl: null },
    });
  });

  it("canonicalizes an encoded dedicated-server URL", () => {
    const link = parseBotLink(
      `?code=AB12CD&server=${encodeURIComponent("wss://Games.Example:443/ws")}`,
    );
    expect(link).toEqual({
      kind: "host",
      seed: expect.objectContaining({ serverUrl: "wss://games.example/ws" }),
    });
  });

  it("keeps the path of a guest link's server URL", () => {
    const link = parseBotLink("?join=AB12CD@wss://lobby.phase-rs.dev/ws");
    expect(link).toEqual({
      kind: "join",
      code: "AB12CD",
      serverUrl: "wss://lobby.phase-rs.dev/ws",
    });
  });

  it.each([
    ["lowercase code", "?code=ab12cd"],
    ["5-character code", "?code=AB12C"],
    ["non-numeric players", "?code=AB12CD&players=four"],
    ["non-websocket server", "?code=AB12CD&server=http://x"],
    ["guest link without a server", "?join=AB12CD"],
    ["guest link with an empty server", "?join=AB12CD@"],
    ["guest link with a non-websocket server", "?join=AB12CD@https://x"],
    ["guest link with a bad code", "?join=ab12cd@wss://lobby.phase-rs.dev/ws"],
    ["both code and join", "?code=AB12CD&join=AB12CD@wss://lobby.phase-rs.dev/ws"],
  ])("rejects a %s", (_label, search) => {
    expect(parseBotLink(search)).toEqual({ kind: "invalid" });
  });
});

describe("hostLinkSearch", () => {
  const seeds: [string, HostSeed][] = [
    [
      "every field",
      {
        code: "AB12CD",
        format: "Commander",
        playerCount: 4,
        roomName: "Friday & #1 night",
        serverUrl: "wss://games.example/ws?region=eu",
      },
    ],
    [
      "no optional field",
      { code: "AB12CD", format: null, playerCount: null, roomName: null, serverUrl: null },
    ],
    [
      "only a server",
      {
        code: "ZZ99ZZ",
        format: null,
        playerCount: null,
        roomName: null,
        serverUrl: "wss://games.example/ws",
      },
    ],
  ];

  it.each(seeds)("round-trips a seed with %s through parseBotLink", (_label, seed) => {
    const search = hostLinkSearch(seed);
    expect(parseBotLink(`?${search}`)).toEqual({ kind: "host", seed });
    expect(search).not.toContain("view=");
    expect(search.startsWith("?")).toBe(false);
  });
});

describe("bot link stash", () => {
  const KEY = "phase:bot-link";
  const JOIN_SEARCH = "?join=AB12CD@wss://x.example/ws";
  const JOIN = { kind: "join", code: "AB12CD", serverUrl: "wss://x.example/ws" };

  beforeEach(() => {
    sessionStorage.clear();
    __resetBotLinkStashForTests();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("is read once per document and survives the read", () => {
    stashBotLink(JOIN_SEARCH);

    expect(readStashedBotLinkOnce()).toEqual(JOIN);
    expect(readStashedBotLinkOnce()).toBeNull();
    expect(sessionStorage.getItem(KEY)).not.toBeNull();

    // A new document reads it again.
    __resetBotLinkStashForTests();
    expect(readStashedBotLinkOnce()).toEqual(JOIN);
  });

  it("expires after ten minutes", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-09-22T12:00:00Z"));
    stashBotLink(JOIN_SEARCH);

    vi.setSystemTime(Date.now() + BOT_LINK_STASH_MAX_AGE_MS - 1);
    expect(readStashedBotLinkOnce()).toEqual(JOIN);

    __resetBotLinkStashForTests();
    vi.setSystemTime(Date.now() + 2);
    expect(readStashedBotLinkOnce()).toBeNull();
    expect(sessionStorage.getItem(KEY)).toBeNull();
  });

  it.each([
    ["unparseable JSON", "{"],
    ["a non-string search", JSON.stringify({ search: 5 })],
    ["a missing timestamp", JSON.stringify({ search: JOIN_SEARCH })],
    ["an invalid link", JSON.stringify({ search: "?code=ab", at: Date.now() })],
    ["no link at all", JSON.stringify({ search: "?view=lobby", at: Date.now() })],
  ])("drops %s", (_label, raw) => {
    sessionStorage.setItem(KEY, raw);

    expect(readStashedBotLinkOnce()).toBeNull();
    expect(sessionStorage.getItem(KEY)).toBeNull();
  });

  it("is gone after clearStashedBotLink", () => {
    stashBotLink(JOIN_SEARCH);
    clearStashedBotLink();

    expect(readStashedBotLinkOnce()).toBeNull();
  });

  it("holds a Continue-anyway decision until the document is reset", () => {
    expect(hasContinuedOnStaleBuild()).toBe(false);
    markContinuedOnStaleBuild();
    expect(hasContinuedOnStaleBuild()).toBe(true);

    __resetBotLinkStashForTests();
    expect(hasContinuedOnStaleBuild()).toBe(false);
  });
});
