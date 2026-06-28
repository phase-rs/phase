import assert from "node:assert/strict";
import test, { beforeEach } from "node:test";

import { handleImportDeck } from "../src/import-deck.ts";

const originalFetch = globalThis.fetch;

beforeEach(() => {
  globalThis.fetch = originalFetch;
});

function mockUpstream(payload, init = {}) {
  globalThis.fetch = async () =>
    new Response(JSON.stringify(payload), {
      status: init.status ?? 200,
      headers: { "Content-Type": "application/json" },
    });
}

function mockUpstreamRaw(status) {
  globalThis.fetch = async () => new Response("", { status });
}

function mockUpstreamNetworkError() {
  globalThis.fetch = async () => {
    throw new TypeError("Failed to fetch");
  };
}

function importRequest(deckUrl, { method = "GET", origin = "http://localhost:5173" } = {}) {
  const url = `https://lobby.example/import-deck?url=${encodeURIComponent(deckUrl)}`;
  return new Request(url, { method, headers: { Origin: origin } });
}

async function call(deckUrl, opts) {
  return handleImportDeck(importRequest(deckUrl, opts), {});
}

// ---------------------------------------------------------------------------
// Preflight + URL validation
// ---------------------------------------------------------------------------

test("OPTIONS preflight returns 204 with CORS headers", async () => {
  const resp = await call("https://moxfield.com/decks/abc", { method: "OPTIONS" });
  assert.equal(resp.status, 204);
  assert.equal(resp.headers.get("Access-Control-Allow-Origin"), "*");
  assert.match(resp.headers.get("Access-Control-Allow-Methods") ?? "", /GET/);
});

test("non-GET, non-OPTIONS method is rejected with method_not_allowed code", async () => {
  const req = new Request("https://lobby.example/import-deck?url=https://moxfield.com/decks/x", { method: "POST" });
  const resp = await handleImportDeck(req, {});
  assert.equal(resp.status, 405);
  const body = await resp.json();
  assert.equal(body.error, "method_not_allowed");
});

test("protocol-less URLs are accepted (https:// is auto-prepended)", async () => {
  mockUpstream({ name: "X", commanders: { a: { quantity: 1, card: { name: "Foo" } } } });
  const req = new Request(
    "https://lobby.example/import-deck?url=" + encodeURIComponent("moxfield.com/decks/x"),
  );
  const resp = await handleImportDeck(req, {});
  assert.equal(resp.status, 200);
});

test("pasted URL wrappers/trailing punctuation are normalized before source parse", async () => {
  mockUpstream({ name: "X", cards: [{ quantity: 1, card: { oracleCard: { name: "Foo" } } }] });
  const resp = await call("<https://archidekt.com/decks/123456/my_deck.>");
  assert.equal(resp.status, 200);
  const resp2 = await call("<https://archidekt.com/decks/123456/my_deck>.");
  assert.equal(resp2.status, 200);
});

test("Moxfield: upstream returns non-JSON body → 502 upstream_unavailable (not 504)", async () => {
  // Maintenance pages and HTML login walls are 2xx + non-JSON. Must be
  // distinct from a network timeout (504) so the user sees a useful error.
  global.fetch = async () =>
    new Response("<!DOCTYPE html><html>maintenance</html>", {
      status: 200,
      headers: { "Content-Type": "text/html" },
    });
  const resp = await call("https://moxfield.com/decks/maint");
  assert.equal(resp.status, 502);
  const body = await resp.json();
  assert.equal(body.error, "upstream_unavailable");
});

test("missing url query parameter returns 400 invalid_url", async () => {
  const req = new Request("https://lobby.example/import-deck");
  const resp = await handleImportDeck(req, {});
  assert.equal(resp.status, 400);
  const body = await resp.json();
  assert.equal(body.error, "invalid_url");
});

test("unsupported source returns 400 unsupported_source", async () => {
  const resp = await call("https://example.com/decks/abc");
  assert.equal(resp.status, 400);
  const body = await resp.json();
  assert.equal(body.error, "unsupported_source");
});

test("malformed URL returns 400 unsupported_source", async () => {
  const resp = await call("not a url");
  assert.equal(resp.status, 400);
});

test("Archidekt with non-numeric id is not recognized as Archidekt", async () => {
  const resp = await call("https://archidekt.com/decks/abc");
  assert.equal(resp.status, 400);
  const body = await resp.json();
  assert.equal(body.error, "unsupported_source");
});

// ---------------------------------------------------------------------------
// Moxfield projection
// ---------------------------------------------------------------------------

test("Moxfield: projects boards onto canonical decklist text with printings", async () => {
  mockUpstream({
    name: "Krenko Goblins",
    commanders: {
      a: { quantity: 1, card: { name: "Krenko, Mob Boss", set: "m19", cn: "145" } },
    },
    mainboard: {
      b: { quantity: 1, card: { name: "Sol Ring", set: "ltc", cn: "280" } },
      c: { quantity: 1, card: { name: "Goblin Chieftain", set: "m10", cn: "139" } },
    },
    sideboard: {},
    companions: {},
  });

  const resp = await call("https://www.moxfield.com/decks/oEWXWHM5");
  assert.equal(resp.status, 200);
  assert.equal(resp.headers.get("Content-Type"), "text/plain; charset=utf-8");
  const text = await resp.text();
  assert.equal(
    text,
    [
      "Name: Krenko Goblins",
      "[Commander]",
      "1 Krenko, Mob Boss (M19) 145",
      "[Main]",
      "1 Sol Ring (LTC) 280",
      "1 Goblin Chieftain (M10) 139",
      "",
    ].join("\n"),
  );
});

test("Moxfield: emits companion section last so deckParser doesn't misfile", async () => {
  mockUpstream({
    name: "Lurrus Aggro",
    mainboard: { a: { quantity: 4, card: { name: "Mishra's Bauble" } } },
    companions: { b: { quantity: 1, card: { name: "Lurrus of the Dream-Den" } } },
  });

  const text = await (await call("https://moxfield.com/decks/xyz")).text();
  assert.match(text, /\[Companion\]\n1 Lurrus of the Dream-Den/);
  assert.ok(text.indexOf("[Main]") < text.indexOf("[Companion]"));
});

test("Moxfield: projects v2 nested boards (boards.<key>.cards)", async () => {
  // The v2 API nests boards under a top-level `boards` map with entries under
  // `.cards`. Reading boards only at the top level silently 404s every import.
  mockUpstream({
    name: "Nested",
    boards: {
      commanders: { cards: { a: { quantity: 1, card: { name: "Krenko, Mob Boss" } } } },
      mainboard: { cards: { b: { quantity: 1, card: { name: "Sol Ring" } } } },
      sideboard: { cards: {} },
      companions: { cards: {} },
    },
  });
  const resp = await call("https://moxfield.com/decks/nested");
  assert.equal(resp.status, 200);
  const text = await resp.text();
  assert.match(text, /\[Commander\]\n1 Krenko, Mob Boss/);
  assert.match(text, /\[Main\]\n1 Sol Ring/);
});

test("Moxfield: maybeboard board is excluded from import", async () => {
  mockUpstream({
    name: "Testing",
    boards: {
      commanders: { cards: {} },
      mainboard: {
        cards: {
          a: { quantity: 1, card: { name: "Sol Ring" } },
        },
      },
      maybeboard: {
        cards: {
          b: { quantity: 1, card: { name: "Mana Crypt" } },
        },
      },
      sideboard: { cards: {} },
      companions: { cards: {} },
    },
  });
  const text = await (await call("https://moxfield.com/decks/maybe")).text();
  assert.match(text, /Sol Ring/);
  assert.ok(!text.includes("Mana Crypt"));
});

test("Moxfield: sideboard-only deck is not rejected as empty", async () => {
  mockUpstream({
    name: "SB only",
    mainboard: {},
    commanders: {},
    sideboard: { a: { quantity: 2, card: { name: "Negate" } } },
  });
  const resp = await call("https://moxfield.com/decks/sbonly");
  assert.equal(resp.status, 200);
  const text = await resp.text();
  assert.match(text, /\[Sideboard\]\n2 Negate/);
});

test("Moxfield: empty deck (private/hidden) returns 404 not_found", async () => {
  mockUpstream({ name: "Hidden", mainboard: {}, commanders: {} });
  const resp = await call("https://moxfield.com/decks/zzz");
  assert.equal(resp.status, 404);
  const body = await resp.json();
  assert.equal(body.error, "not_found");
  assert.equal(body.source, "moxfield");
});

test("Moxfield: upstream 404 surfaces as 404 not_found", async () => {
  mockUpstreamRaw(404);
  const resp = await call("https://moxfield.com/decks/missing");
  assert.equal(resp.status, 404);
  const body = await resp.json();
  assert.equal(body.error, "not_found");
});

test("Moxfield: upstream 500 surfaces as 502 upstream_unavailable", async () => {
  mockUpstreamRaw(500);
  const resp = await call("https://moxfield.com/decks/oops");
  assert.equal(resp.status, 502);
  const body = await resp.json();
  assert.equal(body.error, "upstream_unavailable");
});

test("Moxfield: network failure surfaces as 504 upstream_timeout", async () => {
  mockUpstreamNetworkError();
  const resp = await call("https://moxfield.com/decks/network");
  assert.equal(resp.status, 504);
  const body = await resp.json();
  assert.equal(body.error, "upstream_timeout");
});

// ---------------------------------------------------------------------------
// Archidekt projection
// ---------------------------------------------------------------------------

test("Archidekt: classifies categories and skips excluded boards", async () => {
  mockUpstream({
    name: "Zimone Combo",
    categories: [
      { name: "Commander", includedInDeck: true },
      { name: "Maybeboard", includedInDeck: false },
    ],
    cards: [
      {
        quantity: 1,
        categories: ["Commander"],
        card: {
          oracleCard: { name: "Zimone, All-Questioning" },
          edition: { editioncode: "dft" },
          collectorNumber: "229",
        },
      },
      {
        quantity: 1,
        categories: ["Lands"],
        card: {
          oracleCard: { name: "Command Tower" },
          edition: { editioncode: "cmr" },
          collectorNumber: "350",
        },
      },
      {
        quantity: 1,
        categories: ["Maybeboard"],
        card: {
          oracleCard: { name: "Mana Crypt" },
          edition: { editioncode: "2xm" },
          collectorNumber: "270",
        },
      },
      {
        quantity: 2,
        categories: ["Sideboard"],
        card: {
          oracleCard: { name: "Negate" },
          edition: { editioncode: "m21" },
          collectorNumber: "55",
        },
      },
    ],
  });

  const text = await (await call("https://archidekt.com/decks/123456/zimone")).text();
  assert.equal(
    text,
    [
      "Name: Zimone Combo",
      "[Commander]",
      "1 Zimone, All-Questioning (DFT) 229",
      "[Main]",
      "1 Command Tower (CMR) 350",
      "[Sideboard]",
      "2 Negate (M21) 55",
      "",
    ].join("\n"),
  );
});

test("Archidekt: card with only excluded categories is dropped", async () => {
  mockUpstream({
    name: "Custom",
    categories: [{ name: "OnlyExcluded", includedInDeck: false }],
    cards: [
      {
        quantity: 1,
        categories: ["OnlyExcluded"],
        card: { oracleCard: { name: "Should Drop" } },
      },
      {
        quantity: 1,
        categories: ["Lands"],
        card: { oracleCard: { name: "Should Keep" } },
      },
    ],
  });

  const text = await (await call("https://archidekt.com/decks/42")).text();
  assert.ok(!text.includes("Should Drop"));
  assert.ok(text.includes("Should Keep"));
});

test("Archidekt: empty/private deck returns 404 not_found", async () => {
  mockUpstream({ name: "Hidden", cards: [], categories: [] });
  const resp = await call("https://archidekt.com/decks/9999");
  assert.equal(resp.status, 404);
  const body = await resp.json();
  assert.equal(body.source, "archidekt");
});

// ---------------------------------------------------------------------------
// Origin allowlist
// ---------------------------------------------------------------------------

test("ALLOWED_ORIGINS: matching origin echoed; non-matching falls back to first listed", async () => {
  mockUpstream({ name: "X", mainboard: { a: { quantity: 1, card: { name: "Forest" } } } });
  const env = { ALLOWED_ORIGINS: "https://phase-rs.dev,http://localhost:5173" };

  const matched = await handleImportDeck(
    importRequest("https://moxfield.com/decks/x", { origin: "http://localhost:5173" }),
    env,
  );
  assert.equal(matched.headers.get("Access-Control-Allow-Origin"), "http://localhost:5173");

  const fallback = await handleImportDeck(
    importRequest("https://moxfield.com/decks/x", { origin: "https://attacker.example" }),
    env,
  );
  assert.equal(fallback.headers.get("Access-Control-Allow-Origin"), "https://phase-rs.dev");
});
