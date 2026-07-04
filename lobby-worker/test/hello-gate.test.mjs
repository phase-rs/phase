import assert from "node:assert/strict";
import { test } from "node:test";

import { classifyHelloGate } from "../src/hello-gate.ts";

test("rejects malformed protocol versions", () => {
  assert.deepEqual(
    classifyHelloGate(
      false,
      { type: "ClientHello", data: { protocol_version: "invalid" } },
      11,
    ),
    { kind: "reject_protocol", client: Number.NaN, server: 11 },
  );
});

test("accepts current and previous protocol versions", () => {
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 10 } }, 11),
    { kind: "accept" },
  );
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 11 } }, 11),
    { kind: "accept" },
  );
});

test("rejects versions outside the supported range", () => {
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 9 } }, 11),
    { kind: "reject_protocol", client: 9, server: 11 },
  );
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 12 } }, 11),
    { kind: "reject_protocol", client: 12, server: 11 },
  );
});
