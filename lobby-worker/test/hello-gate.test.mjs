import assert from "node:assert/strict";
import { test } from "node:test";

import { classifyHelloGate } from "../src/hello-gate.ts";

/** A broker speaking full-game protocol 11 and lobby protocol 1. */
const POLICY = {
  serverProtocolVersion: 11,
  lobbyProtocolVersion: 1,
  minSupportedLobbyProtocol: 1,
};

const hello = (data) => ({ type: "ClientHello", data });

// ── Legacy path: clients that predate the lobby-owned version ──────────────
// These must behave EXACTLY as they did before the split.

test("rejects malformed protocol versions", () => {
  assert.deepEqual(classifyHelloGate(false, hello({ protocol_version: "invalid" }), POLICY), {
    kind: "reject_protocol",
    client: Number.NaN,
    server: 11,
  });
});

test("accepts current and previous protocol versions", () => {
  assert.deepEqual(classifyHelloGate(false, hello({ protocol_version: 10 }), POLICY), {
    kind: "accept",
  });
  assert.deepEqual(classifyHelloGate(false, hello({ protocol_version: 11 }), POLICY), {
    kind: "accept",
  });
});

test("rejects versions outside the supported range", () => {
  assert.deepEqual(classifyHelloGate(false, hello({ protocol_version: 9 }), POLICY), {
    kind: "reject_protocol",
    client: 9,
    server: 11,
  });
  assert.deepEqual(classifyHelloGate(false, hello({ protocol_version: 12 }), POLICY), {
    kind: "reject_protocol",
    client: 12,
    server: 11,
  });
});

// ── Lobby path: clients that declare their lobby protocol ──────────────────

test("a stale full-game protocol is accepted when the lobby version is current", () => {
  // The exact shape that took preview multiplayer down: the client's full-game
  // number is many bumps behind the broker, but the lobby surface they both
  // speak is identical. Gating on protocol_version rejected this.
  const frame = hello({ protocol_version: 2, lobby_protocol_version: 1 });
  assert.deepEqual(classifyHelloGate(false, frame, POLICY), { kind: "accept" });
});

test("no ceiling: a lobby version newer than the broker is accepted", () => {
  // An unknown lobby variant is already rejected per-frame by the Rust core, so
  // refusing the whole connection would evict a client over a variant it may
  // never send.
  const frame = hello({ protocol_version: 11, lobby_protocol_version: 99 });
  assert.deepEqual(classifyHelloGate(false, frame, POLICY), { kind: "accept" });
});

test("the lobby floor is still enforced", () => {
  const frame = hello({ protocol_version: 11, lobby_protocol_version: 0 });
  assert.deepEqual(classifyHelloGate(false, frame, { ...POLICY, minSupportedLobbyProtocol: 2 }), {
    kind: "reject_protocol",
    client: 0,
    server: 1,
  });
});

test("lobby_protocol_version 0 is a declaration, not an absent field", () => {
  // Presence, not truthiness. A client declaring 0 takes the lobby path (and is
  // accepted at floor 0); only an ABSENT field falls back to the legacy window.
  // Conflating the two would route a declaring client through a protocol_version
  // check it deliberately opted out of.
  const frame = hello({ protocol_version: 2, lobby_protocol_version: 0 });
  assert.deepEqual(classifyHelloGate(false, frame, { ...POLICY, minSupportedLobbyProtocol: 0 }), {
    kind: "accept",
  });
});

// ── Frame ordering, unchanged by the split ────────────────────────────────

test("non-hello frames before the handshake are rejected", () => {
  assert.deepEqual(classifyHelloGate(false, { type: "SubscribeLobby" }, POLICY), {
    kind: "reject_handshake",
  });
});

test("a redundant hello is ignored and regular frames pass through", () => {
  assert.deepEqual(classifyHelloGate(true, hello({}), POLICY), { kind: "ignore" });
  assert.deepEqual(classifyHelloGate(true, { type: "SubscribeLobby" }, POLICY), { kind: "pass" });
});
