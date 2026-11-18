import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

// Ordering guardrail for `LobbyDO.webSocketMessage`: a dirty frame's snapshot
// must be written BEFORE its outbounds are delivered.
//
// The broker admits a growth write only when the resulting value fits the
// persisted ceiling (`Broker::handle`'s capacity boundary), and a refusal is an
// ordinary error outbound. The reply is therefore only a faithful "this
// succeeded" if the snapshot it describes has been stored first; sending it
// ahead of the `put` acknowledged writes whose persistence had not been
// attempted. Read as SOURCE TEXT for the same reason as
// `conn-attachment.test.mjs`: `lobby-do.ts` imports a gitignored wasm package at
// module scope, so it cannot be imported by a plain checkout's CI.

const HERE = dirname(fileURLToPath(import.meta.url));
const SOURCE = readFileSync(resolve(HERE, "../src/lobby-do.ts"), "utf8");

/** The body between `header`'s opening `{` and its brace-depth-matched close. */
function methodBody(header) {
  const start = SOURCE.indexOf(header);
  assert.notEqual(start, -1, `expected to find \`${header}\``);
  const bodyStart = SOURCE.indexOf("{", start);
  let depth = 0;
  for (let index = bodyStart; index < SOURCE.length; index += 1) {
    if (SOURCE[index] === "{") depth += 1;
    if (SOURCE[index] === "}") {
      depth -= 1;
      if (depth === 0) return SOURCE.slice(bodyStart + 1, index);
    }
  }
  throw new Error(`expected \`${header}\` to be closed by a matching brace`);
}

test("webSocketMessage persists a dirty snapshot before delivering its outbounds", () => {
  const body = methodBody("async webSocketMessage(ws: WebSocket, raw: string | ArrayBuffer)");
  const put = body.indexOf("this.persistSnapshotIfFits(broker)");
  assert.notEqual(put, -1, "expected the snapshot write in webSocketMessage");

  // The reject branch answers a frame that mutated nothing, so it replies early;
  // every other reply is the final `interpret` of the success path.
  const reply = body.lastIndexOf("this.interpret(ws, result.outbounds)");
  assert.notEqual(reply, -1, "expected the outbound delivery in webSocketMessage");
  assert.ok(
    put < reply,
    "the snapshot write must precede the reply: a success reply has to describe stored state",
  );
});

test("every snapshot write is guarded by the broker's storage-fit authority", () => {
  // The only raw `put` of the snapshot lives in the guarded helper; the message,
  // alarm and disconnect paths all reach storage through it.
  const raw = SOURCE.match(/storage\.put\(SNAPSHOT_KEY/g) ?? [];
  assert.equal(raw.length, 1, "exactly one snapshot `put` may exist");

  const helper = methodBody("private async persistSnapshotIfFits(broker: WasmBroker)");
  const guard = helper.indexOf("broker.snapshot_fits_storage()");
  const put = helper.indexOf("this.ctx.storage.put(SNAPSHOT_KEY, broker.snapshot())");
  assert.notEqual(guard, -1, "the helper must ask the broker whether the snapshot fits");
  assert.ok(guard < put, "the fit check must precede the put");
  assert.ok(helper.includes("lobby_snapshot_unstorable"), "an unstorable snapshot is logged loudly");

  const callers = SOURCE.match(/this\.persistSnapshotIfFits\(broker\)/g) ?? [];
  assert.ok(callers.length >= 3, "message, alarm and disconnect paths all use the helper");
});
