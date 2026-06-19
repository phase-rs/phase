import assert from "node:assert/strict";
import { test } from "node:test";

import { classifyHelloGate } from "../src/hello-gate.ts";

test("rejects malformed protocol versions", () => {
  assert.deepEqual(
    classifyHelloGate(
      false,
      { type: "ClientHello", data: { protocol_version: "invalid" } },
      8,
    ),
    { kind: "reject_protocol", client: Number.NaN, server: 8 },
  );
});

test("accepts current and previous protocol versions", () => {
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 7 } }, 8),
    { kind: "accept" },
  );
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 8 } }, 8),
    { kind: "accept" },
  );
});

test("rejects versions outside the supported range", () => {
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 6 } }, 8),
    { kind: "reject_protocol", client: 6, server: 8 },
  );
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 9 } }, 8),
    { kind: "reject_protocol", client: 9, server: 8 },
  );
});
