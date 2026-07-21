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

test("sanitizeTelemetryBatch accepts the Tier 2 report/usage events with their columns", () => {
  const out = sanitizeTelemetryBatch(
    batch([
      { event: "card_report", oracle_id: "abc", face_name: "Front", name: "Colossal Dreadmaw", zone: "Battlefield", game_mode: "ai", turn: 5, supported: 2, total: 3 },
      { event: "session_start", route: "/game" },
      { event: "game_start", game_mode: "ai", format: "HistoricBrawl", player_count: 2, ai_count: 1 },
      { event: "route_view", route: "/deck-builder" },
    ]),
  );
  assert.equal(out.length, 4);
  for (const e of out) {
    assert.equal(e.blobs.length, EVENT_SCHEMAS[e.event].blobs.length);
    assert.equal(e.doubles.length, EVENT_SCHEMAS[e.event].doubles.length);
  }
  const [report, session, gameStart, route] = out;
  // card_report columns land in documented order (blobs then doubles).
  assert.deepEqual(report.blobs, ["abc", "Front", "Colossal Dreadmaw", "Battlefield", "ai"]);
  assert.deepEqual(report.doubles, [5, 2, 3]);
  assert.deepEqual(session.blobs, ["/game"]);
  assert.deepEqual(session.doubles, []);
  assert.deepEqual(gameStart.blobs, ["ai", "HistoricBrawl"]);
  assert.deepEqual(gameStart.doubles, [2, 1]);
  assert.deepEqual(route.blobs, ["/deck-builder"]);
  assert.deepEqual(route.doubles, []);
});

test("unknown fields are dropped from a new Tier 2 event", () => {
  const [e] = sanitizeTelemetryBatch(
    batch([{ event: "card_report", oracle_id: "abc", face_name: "", name: "X", zone: "Hand", game_mode: "ai", turn: 1, supported: 1, total: 1, secret: "leak" }]),
  );
  // Only schema columns are projected; the extra field never appears.
  assert.deepEqual(e.blobs, ["abc", "", "X", "Hand", "ai"]);
  assert.deepEqual(e.doubles, [1, 1, 1]);
});

test("toDataPoint prepends the envelope blobs and sets the single index", () => {
  const [e] = sanitizeTelemetryBatch(
    batch([{ event: "chunk_reload", reason: "preload-error", deferred: true, chunk: "c.js" }]),
  );
  const point = toDataPoint(e);
  assert.deepEqual(point.indexes, ["chunk_reload"]);
  // blobs: [event, app_version, build_hash, platform, ...schema.blobs].
  // Absent probe_* fields project as ""/0 in their appended positions.
  assert.deepEqual(point.blobs, [
    "chunk_reload",
    "0.42.1",
    "02b26c3",
    "web",
    "preload-error",
    "c.js",
    "",
    "",
  ]);
  // deferred=true coerces to 1.
  assert.deepEqual(point.doubles, [1, 0, 0]);
  assert.equal(point.indexes.length, 1);
  assert.ok(point.blobs.length <= 20);
  assert.ok(point.doubles.length <= 20);
});

test("chunk_reload probe fields land in the appended columns", () => {
  const [e] = sanitizeTelemetryBatch(
    batch([
      {
        event: "chunk_reload",
        reason: "loop-abort",
        deferred: false,
        chunk: "Failed to fetch dynamically imported module: https://x/a.js",
        probe_cache: "HIT",
        probe_ray: "8f3a2-SJC",
        probe_status: 200,
        probe_sw: 1,
      },
    ]),
  );
  const point = toDataPoint(e);
  assert.deepEqual(point.blobs.slice(4), [
    "loop-abort",
    "Failed to fetch dynamically imported module: https://x/a.js",
    "HIT",
    "8f3a2-SJC",
  ]);
  assert.deepEqual(point.doubles, [0, 200, 1]);
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
  // winner_kind null → ""; array keeps only strings, comma-joined;
  // pending_trigger_abandons absent → "".
  assert.deepEqual(drawn.blobs, ["draw", "", "ai", "x,y", ""]);
  assert.deepEqual(drawn.doubles, [7]);
});

test("the game_end pending_trigger_abandons list survives the join at the client's 20-item cap", () => {
  // Mirrors the unimplemented_oracle_ids blob: the abandon descriptors are
  // comma-joined into the 5th game_end blob and kept up to the list cap.
  const abandons = Array.from(
    { length: 20 },
    (_, i) => `Test Source (stack entry ${i})`,
  );
  const [e] = sanitizeTelemetryBatch(
    batch([{ event: "game_end", result: "winner", winner_kind: "human", game_mode: "ai", turn_count: 3, pending_trigger_abandons: abandons }]),
  );
  assert.equal(e.blobs[4], abandons.join(","));
  assert.equal(e.blobs[4].split(",").length, 20);
  // Non-strings are dropped, strings comma-joined (same treatment as oracle ids).
  const [mixed] = sanitizeTelemetryBatch(
    batch([{ event: "game_end", result: "draw", winner_kind: null, game_mode: "ai", turn_count: 1, pending_trigger_abandons: ["a", 5, "b"] }]),
  );
  assert.equal(mixed.blobs[4], "a,b");
});
