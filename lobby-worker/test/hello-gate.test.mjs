import assert from "node:assert/strict";
import { test } from "node:test";

import { classifyHelloGate } from "../src/hello-gate.ts";

test("rejects malformed protocol versions", () => {
  assert.deepEqual(
    classifyHelloGate(
      false,
      { type: "ClientHello", data: { protocol_version: "invalid" } },
      10,
    ),
    { kind: "reject_protocol", client: Number.NaN, server: 10 },
  );
});

test("accepts current and previous protocol versions", () => {
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 9 } }, 10),
    { kind: "accept" },
  );
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 10 } }, 10),
    { kind: "accept" },
  );
});

test("rejects versions outside the supported range", () => {
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 8 } }, 10),
    { kind: "reject_protocol", client: 8, server: 10 },
  );
  assert.deepEqual(
    classifyHelloGate(false, { type: "ClientHello", data: { protocol_version: 11 } }, 10),
    { kind: "reject_protocol", client: 11, server: 10 },
  );
});
