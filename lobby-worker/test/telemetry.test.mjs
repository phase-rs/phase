import assert from "node:assert/strict";
import { test } from "node:test";

import {
  EVENT_SCHEMAS,
  MAX_EVENTS_PER_BATCH,
  sanitizeTelemetryBatch,
  toDataPoint,
} from "../src/telemetry.ts";

/** Minimal valid envelope helper. */
function batch(events, overrides = {}) {
  return {
    schema: 1,
    app_version: "0.42.1",
    build_hash: "02b26c3",
    platform: "web",
    events,
    ...overrides,
  };
}

test("sanitizeTelemetryBatch accepts the five Tier 1 events with envelope fields", () => {
  const out = sanitizeTelemetryBatch(
    batch([
      { event: "engine_panic", fatal: true, reason: "STATE_LOST", panic: "boom", game_mode: "ai", turn: 4 },
      { event: "stuck_decision", waiting_for_kind: "Priority", game_mode: "ai", phase: "Upkeep" },
      { event: "js_error", name: "TypeError", message: "x", top_frame: "at f", source: "boundary", route: "/game" },
      { event: "chunk_reload", reason: "preload-error", deferred: false, chunk: "index-abc.js" },
      { event: "game_end", result: "winner", winner_kind: "ai", game_mode: "ai", turn_count: 12, unimplemented_oracle_ids: ["a", "b"] },
    ]),
  );
  assert.equal(out.length, 5);
  for (const e of out) {
    assert.equal(e.appVersion, "0.42.1");
    assert.equal(e.buildHash, "02b26c3");
    assert.equal(e.platform, "web");
    assert.equal(e.blobs.length, EVENT_SCHEMAS[e.event].blobs.length);
    assert.equal(e.doubles.length, EVENT_SCHEMAS[e.event].doubles.length);
  }
});

test("toDataPoint prepends the envelope blobs and sets the single index", () => {
  const [e] = sanitizeTelemetryBatch(
    batch([{ event: "chunk_reload", reason: "preload-error", deferred: true, chunk: "c.js" }]),
  );
  const point = toDataPoint(e);
  assert.deepEqual(point.indexes, ["chunk_reload"]);
  // blobs: [event, app_version, build_hash, platform, ...schema.blobs]
  assert.deepEqual(point.blobs, ["chunk_reload", "0.42.1", "02b26c3", "web", "preload-error", "c.js"]);
  // deferred=true coerces to 1.
  assert.deepEqual(point.doubles, [1]);
  assert.equal(point.indexes.length, 1);
  assert.ok(point.blobs.length <= 20);
  assert.ok(point.doubles.length <= 20);
});

test("unknown events are dropped, not errors", () => {
  const out = sanitizeTelemetryBatch(
    batch([
      { event: "not_a_real_event", foo: "bar" },
      { event: "stuck_decision", waiting_for_kind: "Priority", game_mode: "ai", phase: "Draw" },
    ]),
  );
  assert.equal(out.length, 1);
  assert.equal(out[0].event, "stuck_decision");
});

test("unknown fields are dropped from a known event", () => {
  const [e] = sanitizeTelemetryBatch(
    batch([{ event: "stuck_decision", waiting_for_kind: "Priority", game_mode: "ai", phase: "Draw", secret: "leak" }]),
  );
  // Only schema blobs are projected; the extra field never appears.
  assert.deepEqual(e.blobs, ["Priority", "ai", "Draw"]);
});

test("batch is truncated to the per-batch cap", () => {
  const events = Array.from({ length: MAX_EVENTS_PER_BATCH + 10 }, () => ({
    event: "js_error",
    name: "E",
    message: "m",
    top_frame: "at f",
    source: "window",
    route: "/",
  }));
  const out = sanitizeTelemetryBatch(batch(events));
  assert.equal(out.length, MAX_EVENTS_PER_BATCH);
});

test("malformed bodies yield an empty batch", () => {
  assert.deepEqual(sanitizeTelemetryBatch(null), []);
  assert.deepEqual(sanitizeTelemetryBatch("nope"), []);
  assert.deepEqual(sanitizeTelemetryBatch(42), []);
  assert.deepEqual(sanitizeTelemetryBatch({ schema: 2, events: [] }), []); // wrong schema
  assert.deepEqual(sanitizeTelemetryBatch({ schema: 1 }), []); // no events array
  assert.deepEqual(sanitizeTelemetryBatch({ schema: 1, events: "x" }), []);
});

test("field strings are capped at 300 and envelope strings at 64", () => {
  const longField = "a".repeat(500);
  const longEnvelope = "b".repeat(200);
  const [e] = sanitizeTelemetryBatch(
    batch([{ event: "engine_panic", fatal: false, reason: longField, panic: "p", game_mode: "ai", turn: 1 }], {
      app_version: longEnvelope,
    }),
  );
  assert.equal(e.blobs[0].length, 300); // reason capped
  assert.equal(e.appVersion.length, 64); // envelope capped
});

test("the game_end oracle-id list survives the join at the client's 20-id cap", () => {
  // 20 UUID-ish ids (36 chars each + commas ≈ 739 chars) must not be truncated:
  // the list blob cap (1000) exceeds the joined length, so every id is retained.
  const ids = Array.from({ length: 20 }, (_, i) => `oracle-${String(i).padStart(29, "0")}`);
  const [e] = sanitizeTelemetryBatch(
    batch([{ event: "game_end", result: "winner", winner_kind: "human", game_mode: "ai", turn_count: 3, unimplemented_oracle_ids: ids }]),
  );
  assert.equal(e.blobs[3], ids.join(","));
  assert.equal(e.blobs[3].split(",").length, 20);
});

test("booleans and numbers coerce; missing/other values default", () => {
  const [e] = sanitizeTelemetryBatch(
    batch([{ event: "engine_panic", fatal: true, reason: "r", panic: "p", game_mode: "ai" }]),
  );
  // fatal=true → 1; turn missing → 0.
  assert.deepEqual(e.doubles, [1, 0]);

  const [drawn] = sanitizeTelemetryBatch(
    batch([{ event: "game_end", result: "draw", winner_kind: null, game_mode: "ai", turn_count: 7, unimplemented_oracle_ids: ["x", 5, "y"] }]),
  );
  // winner_kind null → ""; array keeps only strings, comma-joined.
  assert.deepEqual(drawn.blobs, ["draw", "", "ai", "x,y"]);
  assert.deepEqual(drawn.doubles, [7]);
});
