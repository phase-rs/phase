import { afterEach, describe, expect, it, vi } from "vitest";

import { fetchStatus, isStatusLive, type StatusMessage } from "../status";

/** A payload that satisfies every field of the contract. Each rejection case
 * below overrides exactly ONE field of this object, so a `null` result is
 * attributable to that field and not to an accidentally-broken baseline. */
const VALID = {
  id: 1756482000000,
  severity: "warning",
  title: "Multiplayer maintenance",
  body: "The lobby restarts at 20:00 UTC. Games in progress are unaffected.",
  dismissible: true,
};

function mockFetch(response: Partial<Response> & { json?: () => Promise<unknown> }) {
  vi.stubGlobal("fetch", vi.fn(() => Promise.resolve(response as Response)));
}

function mockPayload(payload: unknown) {
  mockFetch({ ok: true, json: () => Promise.resolve(payload) });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("fetchStatus", () => {
  it("returns the published message when the payload satisfies the contract", async () => {
    mockPayload(VALID);
    expect(await fetchStatus()).toEqual(VALID);
  });

  it("keeps the optional expiresAt and link fields", async () => {
    const full = {
      ...VALID,
      expiresAt: "2026-08-30T21:00:00Z",
      link: { url: "https://discord.gg/example", label: "Details on Discord" },
    };
    mockPayload(full);
    expect(await fetchStatus()).toEqual(full);
  });

  it("resolves to null when nothing is published (404)", async () => {
    mockFetch({ ok: false, status: 404 });
    expect(await fetchStatus()).toBeNull();
  });

  it("resolves to null when the network rejects", async () => {
    vi.stubGlobal("fetch", vi.fn(() => Promise.reject(new Error("offline"))));
    expect(await fetchStatus()).toBeNull();
  });

  it("resolves to null on malformed JSON", async () => {
    mockFetch({ ok: true, json: () => Promise.reject(new SyntaxError("Unexpected token")) });
    expect(await fetchStatus()).toBeNull();
  });

  // One uniform rule: any contract violation means the payload is not a
  // StatusMessage, so there is nothing to render. No field is ever dropped and
  // the rest rendered.
  const violations: Array<[string, unknown]> = [
    ["an unknown severity", { ...VALID, severity: "urgent" }],
    ["a missing title", { ...VALID, title: undefined }],
    ["an empty title", { ...VALID, title: "" }],
    ["a whitespace-only title", { ...VALID, title: "   " }],
    // The load-bearing one: React throws on a non-string child, and the app's
    // <ErrorBoundary> wraps the whole <Routes> tree — one bad field here would
    // otherwise blank the entire app on / and /multiplayer.
    ["a non-string body", { ...VALID, body: { text: "oops" } }],
    ["a non-number id", { ...VALID, id: "1756482000000" }],
    ["a non-boolean dismissible", { ...VALID, dismissible: "false" }],
    ["a link missing its url", { ...VALID, link: { label: "Details" } }],
    ["a link missing its label", { ...VALID, link: { url: "https://example.com" } }],
    ["a non-object link", { ...VALID, link: "https://example.com" }],
    ["an unparseable expiresAt", { ...VALID, expiresAt: "tomorrow" }],
    ["a non-object payload", "maintenance"],
  ];

  for (const [label, payload] of violations) {
    it(`resolves to null for ${label}`, async () => {
      mockPayload(payload);
      expect(await fetchStatus()).toBeNull();
    });
  }
});

describe("isStatusLive", () => {
  const now = Date.parse("2026-08-29T12:00:00Z");
  const message = VALID as StatusMessage;

  it("is false once expiresAt has passed", () => {
    expect(isStatusLive({ ...message, expiresAt: "2026-08-29T11:59:00Z" }, now)).toBe(false);
  });

  it("is true while expiresAt is still in the future", () => {
    expect(isStatusLive({ ...message, expiresAt: "2026-08-29T12:01:00Z" }, now)).toBe(true);
  });

  it("is true when no expiry was published", () => {
    expect(isStatusLive(message, now)).toBe(true);
  });
});
