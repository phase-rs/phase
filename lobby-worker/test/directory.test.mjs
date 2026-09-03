import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import {
  ANNOUNCE_TIMEOUT_MS,
  buildDirectoryResponse,
  checkIngestGate,
  DIRECTORY_READ_CORS,
  DIRECTORY_WRITE_CORS,
  directoryCorsHeaders,
  foldMetricReports,
  isServerLive,
  partitionServerRows,
  planAnnounceUpsert,
  readBoundedText,
  recordAnnouncedPlayers,
  sanitizeMetricsBatch,
  serverProbeEvents,
  shouldKeepAlarm,
  storedRowFromAnnouncement,
} from "../src/directory.ts";

// Rust's constants arrive as arguments in production, read from the wasm
// exports by lobby-do.ts. These fixture values are deliberately DIFFERENT
// from Rust's in all three dimensions — a 45-minute bucket instead of an
// hour, a 15-hour window instead of a day, and a 4-cell histogram instead of
// an 8-cell one — so a fold that ignored its parameters and reached for a
// literal produces different numbers here rather than coinciding with the
// expectations.
//
// Differing constants are necessary but not sufficient, and two rows carry
// the rest of that weight: V-U13a folds the same reports a second time
// against `[100]`, and V-U13g folds them against `BUCKET_MS / 2`. Those two
// comparisons are what prove the values are READ per call rather than
// captured once; deleting either would leave the discrimination resting on
// these constants alone.
const BUCKET_MS = 2_700_000; // 45 min — Rust's SCORE_BUCKET_MS is 3_600_000
const WINDOW_MS = 20 * BUCKET_MS; // 15 h — Rust's SCORE_WINDOW_MS is 24 h
const EDGES = [25, 75, 150]; // 4 cells — Rust's RTT_BUCKET_EDGES_MS has 8

/** A zeroed histogram of the fixture's cell count, so a hand-built bucket
 *  cannot silently disagree with `EDGES` about its length. */
function zeros() {
  return new Array(EDGES.length + 1).fill(0);
}

/** A `Valid` verdict's announcement payload, with a distinct sentinel in every
 *  field so a transposition fails as loudly as a drop. */
function announcement(overrides = {}) {
  return {
    directory_version: 1,
    url: "wss://a.example/ws",
    name: "b-name",
    mode: "Full",
    server_version: "c-1.2.3",
    protocol_version: 55,
    lobby_protocol_version: 4,
    current_players: 7,
    ...overrides,
  };
}

function row(overrides = {}) {
  return {
    url: "wss://a.example/ws",
    name: "b-name",
    mode: "Full",
    server_version: "c-1.2.3",
    protocol_version: 55,
    lobby_protocol_version: 4,
    current_players: 7,
    first_seen_ms: 1_000,
    last_seen_ms: 2_000,
    ...overrides,
  };
}

function streamOf(...chunks) {
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(chunk);
      controller.close();
    },
  });
}

function bytes(n, fill = "x") {
  return new TextEncoder().encode(fill.repeat(n));
}

// ── V-U7a: the upsert projection is COMPLETE ───────────────────────────────
// The only gate on it. `npm run typecheck` cannot see a property missing from
// an object literal that becomes a SQL bind list, and the design deliberately
// gave up the Rust type that would have.

test("V-U7a: a Match verdict projects all nine columns, none transposed", () => {
  const plan = planAnnounceUpsert({
    announcement: announcement(),
    comparison: { kind: "Match" },
    firstSeenMs: 1_000,
    lastSeenMs: 2_000,
  });
  assert.equal(plan.kind, "upsert");
  // Total over the object: this single assertion fails for a dropped key, a
  // transposed value AND an extra key, because every sentinel is distinct.
  assert.deepEqual(plan.row, {
    url: "wss://a.example/ws",
    name: "b-name",
    mode: "Full",
    server_version: "c-1.2.3",
    protocol_version: 55,
    lobby_protocol_version: 4,
    current_players: 7,
    first_seen_ms: 1_000,
    last_seen_ms: 2_000,
  });
  // `directory_version` is the announcement's version GATE and the response
  // envelope's field — never a column.
  assert.equal("directory_version" in plan.row, false);
  // The shaper is the same function the planner uses, asserted directly so a
  // future caller cannot get a different projection.
  assert.deepEqual(storedRowFromAnnouncement(announcement(), 1_000, 2_000), plan.row);
});

// ── V-U7b/c: a verdict that is not a Match writes nothing ──────────────────

test("V-U7b: each mismatch field rejects with that field and no row", () => {
  for (const field of ["Mode", "ServerVersion", "ProtocolVersion", "LobbyProtocolVersion"]) {
    const plan = planAnnounceUpsert({
      announcement: announcement(),
      comparison: { kind: "Mismatch", field },
      firstSeenMs: 1_000,
      lastSeenMs: 2_000,
    });
    // Asserted per field, not as "some mismatch": the four values cross the
    // wasm boundary as JSON strings, so a handler that collapsed them would
    // still typecheck and still be wrong.
    assert.deepEqual(plan, { kind: "reject", reason: "mismatch", field });
    assert.equal("row" in plan, false);
  }
});

test("V-U7c: an Invalid verdict rejects with no row and no field", () => {
  const plan = planAnnounceUpsert({
    announcement: announcement(),
    comparison: { kind: "Invalid", error: "whatever the core said" },
    firstSeenMs: 1_000,
    lastSeenMs: 2_000,
  });
  assert.deepEqual(plan, { kind: "reject", reason: "invalid" });
});

// ── V-U7d/e: the ingest gate ───────────────────────────────────────────────

test("V-U7d: an oversize body is refused WITHOUT consulting the limiter", async () => {
  let calls = 0;
  const limiter = {
    async limit() {
      calls += 1;
      return { success: true };
    },
  };

  const tooLarge = await checkIngestGate({
    contentLength: 4097,
    maxBytes: 4096,
    key: "announce:1.2.3.4",
    limiter,
  });
  assert.deepEqual(tooLarge, { kind: "reject", reason: "too_large" });
  assert.equal(calls, 0, "the limiter must not be consulted for an oversize body");

  // Paired positive on the SAME stub: exactly at the cap the body is fine and
  // the limiter IS consulted, so the zero above is an ordering fact rather
  // than a stub that never fires.
  const atCap = await checkIngestGate({
    contentLength: 4096,
    maxBytes: 4096,
    key: "announce:1.2.3.4",
    limiter,
  });
  assert.deepEqual(atCap, { kind: "accept" });
  assert.equal(calls, 1);
});

test("V-U7e: limiter refusal and limiter absence differ", async () => {
  const refusing = { async limit() { return { success: false }; } };
  assert.deepEqual(
    await checkIngestGate({ contentLength: 10, maxBytes: 4096, key: "k", limiter: refusing }),
    { kind: "reject", reason: "rate_limited" },
  );

  // Absent binding fails OPEN — a deploy without the binding still serves.
  assert.deepEqual(
    await checkIngestGate({ contentLength: 10, maxBytes: 4096, key: "k" }),
    { kind: "accept" },
  );

  const allowing = { async limit() { return { success: true }; } };
  assert.deepEqual(
    await checkIngestGate({ contentLength: 10, maxBytes: 4096, key: "k", limiter: allowing }),
    { kind: "accept" },
  );

  // An absent Content-Length must not be read as 0-and-fine only by accident:
  // it still reaches the limiter.
  assert.deepEqual(
    await checkIngestGate({ contentLength: null, maxBytes: 4096, key: "k", limiter: refusing }),
    { kind: "reject", reason: "rate_limited" },
  );
});

// ── V-U7g: the 4 KB bound is on the DECODED body ───────────────────────────

test("V-U7g: readBoundedText bounds the decoded text and cancels the stream", async () => {
  // Two chunks each under the cap whose SUM is over it: a per-chunk check
  // would accept this.
  assert.equal(await readBoundedText(streamOf(bytes(3000), bytes(3000)), 4096), null);

  // Paired positives, including the boundary. The bound is `>`, so exactly
  // maxBytes is accepted.
  assert.equal(await readBoundedText(streamOf(bytes(15, "a")), 4096), "a".repeat(15));
  assert.equal((await readBoundedText(streamOf(bytes(4096)), 4096)).length, 4096);
  assert.equal(await readBoundedText(streamOf(bytes(4097)), 4096), null);

  // A multi-byte code point split across chunks must survive: the decoder is
  // streaming, so a naive per-chunk decode would produce U+FFFD here.
  const euro = new TextEncoder().encode("€");
  const split = await readBoundedText(streamOf(euro.slice(0, 1), euro.slice(1)), 4096);
  assert.equal(split, "€");

  // The cap is BYTES, not characters, and this is the pair that tells them
  // apart: 2000 euro signs are 2000 UTF-16 code units but 6000 UTF-8 bytes,
  // so a `.length` bound would accept nearly three times the bytes its name
  // promises. Paired with a string of the same character count that fits.
  const wide = new TextEncoder().encode("€".repeat(2000));
  assert.equal(wide.byteLength, 6000);
  assert.equal(await readBoundedText(streamOf(wide), 4096), null);
  const narrow = new TextEncoder().encode("€".repeat(1000));
  assert.equal(narrow.byteLength, 3000);
  assert.equal((await readBoundedText(streamOf(narrow), 4096)).length, 1000);
});

// ── V-U16: liveness, reaping, alarm ────────────────────────────────────────

test("V-U16a: the reaper fold splits on the announce timeout, both directions", () => {
  const now = 10 * WINDOW_MS;
  const fresh = row({ url: "wss://fresh.example/ws", last_seen_ms: now - (ANNOUNCE_TIMEOUT_MS - 1_000) });
  const stale = row({ url: "wss://stale.example/ws", last_seen_ms: now - (ANNOUNCE_TIMEOUT_MS + 1_000) });
  const { live, expired } = partitionServerRows([fresh, stale], now);
  assert.deepEqual(live.map((r) => r.url), ["wss://fresh.example/ws"]);
  assert.deepEqual(expired.map((r) => r.url), ["wss://stale.example/ws"]);

  // The predicate itself, at its boundary: exactly at the timeout is still
  // live, one ms past is not.
  assert.equal(isServerLive(now - ANNOUNCE_TIMEOUT_MS, now), true);
  assert.equal(isServerLive(now - ANNOUNCE_TIMEOUT_MS - 1, now), false);
});

test("V-U16d: 'anything left to reap' is one predicate over both kinds", () => {
  // An empty broker with directory rows must KEEP the alarm — this is the row
  // that makes the shell's one-line call typecheck-covered.
  assert.equal(shouldKeepAlarm(true, 0), false);
  assert.equal(shouldKeepAlarm(true, 1), true);
  assert.equal(shouldKeepAlarm(false, 0), true);
  assert.equal(shouldKeepAlarm(false, 3), true);
});

// ── V-U17: GET /servers = live ∩ allowlist ─────────────────────────────────

test("V-U16c + V-U17a/b: a row is listed only when it is BOTH live and allowed", () => {
  const now = 10 * WINDOW_MS;
  const listed = row({ url: "wss://listed.example/ws", last_seen_ms: now - 10_000 });
  const dead = row({ url: "wss://dead.example/ws", last_seen_ms: now - 200_000 });
  const unlisted = row({ url: "wss://unlisted.example/ws", last_seen_ms: now - 10_000 });

  const { body } = buildDirectoryResponse({
    rows: [listed, dead, unlisted],
    // `dead` IS allowed and still must not appear (V-U16c/V-U17b); `unlisted`
    // IS live and still must not appear (V-U17a).
    allowlist: new Set(["wss://listed.example/ws", "wss://dead.example/ws"]),
    scores: new Map(),
    directoryVersion: 1,
    nowMs: now,
  });

  assert.deepEqual(body.servers.map((s) => s.url), ["wss://listed.example/ws"]);
});

test("V-U17d: an absent allowlist lists nothing, not everything", () => {
  const now = 10 * WINDOW_MS;
  const rows = [
    row({ url: "wss://one.example/ws", last_seen_ms: now }),
    row({ url: "wss://two.example/ws", last_seen_ms: now }),
  ];

  const closed = buildDirectoryResponse({
    rows,
    allowlist: new Set(),
    scores: new Map(),
    directoryVersion: 1,
    nowMs: now,
  });
  assert.equal(closed.body.servers.length, 0);

  // Paired positive: the same live rows list normally once allowed, so the
  // zero above is the fail-closed direction rather than a broken fixture.
  const open = buildDirectoryResponse({
    rows,
    allowlist: new Set(["wss://one.example/ws", "wss://two.example/ws"]),
    scores: new Map(),
    directoryVersion: 1,
    nowMs: now,
  });
  assert.equal(open.body.servers.length, 2);
});

test("V-U17c: the envelope carries the INJECTED version and the read CORS record", () => {
  const now = 10 * WINDOW_MS;
  const args = {
    rows: [row({ last_seen_ms: now })],
    allowlist: new Set(["wss://a.example/ws"]),
    scores: new Map(),
    nowMs: now,
  };

  // A non-1 sentinel: a hard-coded `1` passes with `directoryVersion: 1` and
  // fails here.
  const first = buildDirectoryResponse({ ...args, directoryVersion: 7 });
  assert.equal(first.body.directory_version, 7);
  const second = buildDirectoryResponse({ ...args, directoryVersion: 9 });
  assert.equal(second.body.directory_version, 9);

  assert.deepEqual(first.headers, {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Methods": "GET, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type",
    "Cache-Control": "public, max-age=60",
  });
});

test("V-U17g: the write CORS record allows POST and the read record does not", () => {
  assert.match(DIRECTORY_WRITE_CORS["Access-Control-Allow-Methods"], /POST/);
  assert.doesNotMatch(DIRECTORY_READ_CORS["Access-Control-Allow-Methods"], /POST/);
  // Paired so ONE shared record cannot satisfy both rows.
  assert.notEqual(
    DIRECTORY_READ_CORS["Access-Control-Allow-Methods"],
    DIRECTORY_WRITE_CORS["Access-Control-Allow-Methods"],
  );
  // The cache header belongs to the read ENDPOINT, not to the CORS helper, so
  // neither record carries one.
  assert.equal("Cache-Control" in DIRECTORY_WRITE_CORS, false);
  assert.equal("Cache-Control" in DIRECTORY_READ_CORS, false);
  assert.deepEqual(directoryCorsHeaders("PUT")["Access-Control-Allow-Methods"], "PUT");
});

test("V-U17f: a listed row's exact wire key set, with and without a score", () => {
  const now = 10 * WINDOW_MS;
  const score = {
    value: 84,
    samples: 110,
    success_rate: 1,
    completion_rate: 0.2,
    median_rtt_ms: 100,
  };
  const scored = buildDirectoryResponse({
    rows: [row({ url: "wss://scored.example/ws", last_seen_ms: now })],
    allowlist: new Set(["wss://scored.example/ws"]),
    scores: new Map([["wss://scored.example/ws", score]]),
    directoryVersion: 1,
    nowMs: now,
  }).body.servers[0];

  // Sorted-key deepEqual fails whether a key is omitted OR added.
  assert.deepEqual(Object.keys(scored).sort(), [
    "current_players",
    "first_seen_ms",
    "last_seen_ms",
    "lobby_protocol_version",
    "mode",
    "name",
    "protocol_version",
    "score",
    "server_version",
    "url",
  ]);
  assert.deepEqual(Object.keys(scored.score).sort(), [
    "completion_rate",
    "median_rtt_ms",
    "samples",
    "success_rate",
    "value",
  ]);
  assert.deepEqual(scored, { ...row({ url: "wss://scored.example/ws", last_seen_ms: now }), score });

  // A genuine second fixture: no evidence must still carry all ten keys with
  // `score === null`. This is what makes "omit the key when there is no
  // score" fail rather than pass.
  const unscored = buildDirectoryResponse({
    rows: [row({ url: "wss://unscored.example/ws", last_seen_ms: now })],
    allowlist: new Set(["wss://unscored.example/ws"]),
    scores: new Map(),
    directoryVersion: 1,
    nowMs: now,
  }).body.servers[0];
  assert.equal(Object.keys(unscored).length, 10);
  assert.equal("score" in unscored, true);
  assert.equal(unscored.score, null);

  // A below-minimum score reaches the wire as an OBJECT with a null `value`,
  // not as a null score — the distinction a consumer's health hints gate on.
  const thin = buildDirectoryResponse({
    rows: [row({ url: "wss://thin.example/ws", last_seen_ms: now })],
    allowlist: new Set(["wss://thin.example/ws"]),
    scores: new Map([
      [
        "wss://thin.example/ws",
        { value: null, samples: 3, success_rate: 1, completion_rate: 0, median_rtt_ms: 100 },
      ],
    ]),
    directoryVersion: 1,
    nowMs: now,
  }).body.servers[0];
  assert.notEqual(thin.score, null);
  assert.equal(thin.score.value, null);
  assert.equal(thin.score.samples, 3);
});

// ── V-U13: metrics ingest ──────────────────────────────────────────────────

const KNOWN_URL = "wss://known.example/ws";

function batch(reports, overrides = {}) {
  return { schema: 1, reports, ...overrides };
}

test("V-U13c: the sanitiser keeps exactly the allow-listed fields", () => {
  const [report] = sanitizeMetricsBatch(
    batch([
      {
        url: KNOWN_URL,
        outcome: "connect_ok",
        rtt_ms: 42,
        admin: true,
        score: 100,
        blobs: ["nope"],
      },
    ]),
  );
  assert.deepEqual(Object.keys(report).sort(), ["outcome", "rtt_ms", "url"]);
  assert.deepEqual(report, {
    url: KNOWN_URL,
    outcome: "connect_ok",
    rtt_ms: 42,
  });

  // Envelope and per-report rejections.
  assert.deepEqual(sanitizeMetricsBatch(batch([], { schema: 2 })), []);
  assert.deepEqual(sanitizeMetricsBatch({ schema: 1, reports: "nope" }), []);
  assert.deepEqual(sanitizeMetricsBatch(null), []);
  assert.deepEqual(sanitizeMetricsBatch(batch([{ url: KNOWN_URL, outcome: "admin_win" }])), []);
  assert.deepEqual(sanitizeMetricsBatch(batch([{ outcome: "connect_ok" }])), []);

  // The batch cap.
  const many = sanitizeMetricsBatch(
    batch(Array.from({ length: 80 }, () => ({ url: KNOWN_URL, outcome: "connect_fail" }))),
  );
  assert.equal(many.length, 50);

  // `rtt_ms` survives ONLY on connect_ok: an RTT on a failed connect is the
  // timing of a handshake that never completed, and folding it would make a
  // server that refuses connections quickly look fast.
  const [failed] = sanitizeMetricsBatch(
    batch([{ url: KNOWN_URL, outcome: "connect_fail", rtt_ms: 5 }]),
  );
  assert.equal("rtt_ms" in failed, false);

  // `game_code` is gated the same way, on the two outcomes that can carry
  // one. A code attached to a connect outcome describes no game — only the
  // game-outcome guard ever reads the field, so keeping it elsewhere would
  // store a value nothing can consume.
  for (const outcome of ["connect_ok", "connect_fail"]) {
    const [connect] = sanitizeMetricsBatch(
      batch([{ url: KNOWN_URL, outcome, game_code: "ABC123" }]),
    );
    assert.equal("game_code" in connect, false, `${outcome} must not keep a game_code`);
  }
  // Paired positive: both game outcomes DO keep it, so the drops above are the
  // gate rather than a sanitiser that lost the field entirely.
  for (const outcome of ["game_completed", "game_abandoned"]) {
    const [game] = sanitizeMetricsBatch(
      batch([{ url: KNOWN_URL, outcome, game_code: "ABC123" }]),
    );
    assert.equal(game.game_code, "ABC123", `${outcome} must keep its game_code`);
  }

  // Clamped and truncated, never trusted. Split across two fixtures because
  // the two fields are gated onto disjoint outcomes.
  const [clamped] = sanitizeMetricsBatch(
    batch([{ url: KNOWN_URL, outcome: "connect_ok", rtt_ms: 9_999_999 }]),
  );
  assert.equal(clamped.rtt_ms, 60_000);
  const [truncated] = sanitizeMetricsBatch(
    batch([{ url: KNOWN_URL, outcome: "game_completed", game_code: "X".repeat(40) }]),
  );
  assert.equal(truncated.game_code.length, 16);
  const [negative] = sanitizeMetricsBatch(
    batch([{ url: KNOWN_URL, outcome: "connect_ok", rtt_ms: -5 }]),
  );
  assert.equal(negative.rtt_ms, 0);
});

test("V-U13a: reports fold into the current bucket's counters", () => {
  const now = 10 * WINDOW_MS + 45 * 60_000;
  const known = new Map([[KNOWN_URL, { buckets: [] }]]);
  const reports = sanitizeMetricsBatch(
    batch([
      { url: KNOWN_URL, outcome: "connect_ok", rtt_ms: 42 },
      { url: KNOWN_URL, outcome: "connect_ok", rtt_ms: 42 },
      { url: KNOWN_URL, outcome: "connect_ok", rtt_ms: 1_200 },
      { url: KNOWN_URL, outcome: "connect_fail" },
    ]),
  );
  const fold = foldMetricReports(reports, known, now, BUCKET_MS, WINDOW_MS, EDGES);

  const [bucket] = fold.counters.get(KNOWN_URL).buckets;
  assert.equal(bucket.connect_attempts, 4);
  assert.equal(bucket.connect_successes, 3);
  assert.equal(bucket.start_ms, now - (now % BUCKET_MS));
  // A latency lands in the first cell whose upper edge it does not exceed.
  // Against this fixture's EDGES ([25, 75, 150]): 42 ms in cell 1 (<= 75),
  // 1200 ms in the overflow cell 3. Rust reads the histogram positionally, so
  // the cell INDEX is the contract, not the millisecond value.
  assert.deepEqual(bucket.rtt_histogram, [0, 2, 0, 1]);
  assert.equal(fold.accepted.length, 4);
  assert.equal(fold.dropped, 0);

  // The edge list is a PARAMETER, not a literal: a different list files the
  // same latencies into different cells and produces a different cell count.
  const coarse = foldMetricReports(reports, known, now, BUCKET_MS, WINDOW_MS, [100]);
  assert.deepEqual(coarse.counters.get(KNOWN_URL).buckets[0].rtt_histogram, [2, 1]);
});

test("V-U13h: the fold does not write through to the caller's counters", () => {
  const now = 10 * WINDOW_MS + 45 * 60_000;
  const startMs = now - (now % BUCKET_MS);
  // A LIVE current bucket with an already-populated histogram — the only shape
  // that can alias, since a fold over an empty `known` has nothing to copy.
  const existing = {
    buckets: [
      {
        start_ms: startMs,
        connect_attempts: 1,
        connect_successes: 1,
        games_started: 0,
        games_completed: 0,
        rtt_histogram: [0, 1, 0, 0],
        announced_players_max: 2,
      },
    ],
  };
  const known = new Map([[KNOWN_URL, existing]]);
  const reports = sanitizeMetricsBatch(
    batch([{ url: KNOWN_URL, outcome: "connect_ok", rtt_ms: 42 }]),
  );

  const first = foldMetricReports(reports, known, now, BUCKET_MS, WINDOW_MS, EDGES);
  // Reach-guard: the report really landed in the pre-existing bucket, so the
  // assertions below are about a copy and not about a fold that did nothing.
  assert.deepEqual(first.counters.get(KNOWN_URL).buckets[0].rtt_histogram, [0, 2, 0, 0]);

  // `known` is INPUT. `rtt_histogram` is the one field the fold mutates in
  // place, which is exactly the one the defensive copy has to reach.
  assert.deepEqual(existing.buckets[0].rtt_histogram, [0, 1, 0, 0]);
  assert.equal(existing.buckets[0].connect_attempts, 1);

  // A second fold over the same input is the same fold, never a double count.
  const second = foldMetricReports(reports, known, now, BUCKET_MS, WINDOW_MS, EDGES);
  assert.deepEqual(second.counters.get(KNOWN_URL).buckets[0].rtt_histogram, [0, 2, 0, 0]);
});

test("V-U13b: a report for a URL with no row is dropped", () => {
  const now = 10 * WINDOW_MS + 45 * 60_000;
  const known = new Map([[KNOWN_URL, { buckets: [] }]]);
  const fold = foldMetricReports(
    sanitizeMetricsBatch(
      batch([
        { url: "wss://forged.example/ws", outcome: "connect_ok", rtt_ms: 10 },
        // Paired positive in the SAME batch: the known URL is still folded, so
        // the drop is the origin guard rather than a fold that did nothing.
        { url: KNOWN_URL, outcome: "connect_ok", rtt_ms: 10 },
      ]),
    ),
    known,
    now,
    BUCKET_MS,
    WINDOW_MS,
    EDGES,
  );
  assert.equal(fold.counters.has("wss://forged.example/ws"), false);
  assert.equal(fold.counters.get(KNOWN_URL).buckets[0].connect_attempts, 1);
  assert.equal(fold.dropped, 1);
  assert.equal(fold.accepted.length, 1);
});

test("V-U13d: a game outcome needs a game code AND an announced-players window", () => {
  const now = 10 * WINDOW_MS + 45 * 60_000;
  const startMs = now - (now % BUCKET_MS);
  const emptyWindow = {
    start_ms: startMs,
    connect_attempts: 0,
    connect_successes: 0,
    games_started: 0,
    games_completed: 0,
    rtt_histogram: zeros(),
    announced_players_max: 0,
  };
  const report = { url: KNOWN_URL, outcome: "game_completed", game_code: "ABC123" };

  // Nobody was ever announced online in this window.
  const noPlayers = foldMetricReports(
    sanitizeMetricsBatch(batch([report])),
    new Map([[KNOWN_URL, { buckets: [emptyWindow] }]]),
    now,
    BUCKET_MS,
    WINDOW_MS,
    EDGES,
  );
  assert.equal(noPlayers.dropped, 1);
  assert.equal(noPlayers.accepted.length, 0);
  // The rejected outcome must not have CREATED the bucket that would have let
  // the next one through.
  assert.equal(noPlayers.counters.has(KNOWN_URL), false);

  // The SAME guard, for the other game outcome. Asserted separately because
  // the two are not interchangeable here: an unannounced `game_abandoned` that
  // slipped past would inflate `games_started` without ever incrementing
  // `games_completed`, which depresses the completion rate of a server that
  // hosted nothing — the anti-spoof direction the guard exists for. A guard
  // written to cover only `game_completed` passes every other assertion in
  // this test.
  const abandonNoPlayers = foldMetricReports(
    sanitizeMetricsBatch(batch([{ ...report, outcome: "game_abandoned" }])),
    new Map([[KNOWN_URL, { buckets: [emptyWindow] }]]),
    now,
    BUCKET_MS,
    WINDOW_MS,
    EDGES,
  );
  assert.equal(abandonNoPlayers.dropped, 1);
  assert.equal(abandonNoPlayers.accepted.length, 0);
  assert.equal(abandonNoPlayers.counters.has(KNOWN_URL), false);

  // A game code is required even when players were online.
  const populated = { ...emptyWindow, announced_players_max: 2 };
  const noCode = foldMetricReports(
    sanitizeMetricsBatch(batch([{ url: KNOWN_URL, outcome: "game_completed" }])),
    new Map([[KNOWN_URL, { buckets: [populated] }]]),
    now,
    BUCKET_MS,
    WINDOW_MS,
    EDGES,
  );
  assert.equal(noCode.dropped, 1);

  // Paired positive: the same report against a window that DID have players.
  const accepted = foldMetricReports(
    sanitizeMetricsBatch(batch([report])),
    new Map([[KNOWN_URL, { buckets: [populated] }]]),
    now,
    BUCKET_MS,
    WINDOW_MS,
    EDGES,
  );
  const bucket = accepted.counters.get(KNOWN_URL).buckets[0];
  assert.equal(bucket.games_started, 1);
  assert.equal(bucket.games_completed, 1);
  assert.equal(accepted.dropped, 0);

  // An abandon counts as started but not completed — that difference IS the
  // completion rate.
  const abandoned = foldMetricReports(
    sanitizeMetricsBatch(batch([{ ...report, outcome: "game_abandoned" }])),
    new Map([[KNOWN_URL, { buckets: [populated] }]]),
    now,
    BUCKET_MS,
    WINDOW_MS,
    EDGES,
  );
  const abandonedBucket = abandoned.counters.get(KNOWN_URL).buckets[0];
  assert.equal(abandonedBucket.games_started, 1);
  assert.equal(abandonedBucket.games_completed, 0);

  // connect_* is deliberately exempt from the guard: a failed connect is
  // precisely the case where nobody was online.
  const connectFail = foldMetricReports(
    sanitizeMetricsBatch(batch([{ url: KNOWN_URL, outcome: "connect_fail" }])),
    new Map([[KNOWN_URL, { buckets: [emptyWindow] }]]),
    now,
    BUCKET_MS,
    WINDOW_MS,
    EDGES,
  );
  assert.equal(connectFail.dropped, 0);
});

test("V-U13f: the announce path RAISES the current window's player peak", () => {
  const now = 10 * WINDOW_MS + 45 * 60_000;
  const startMs = now - (now % BUCKET_MS);
  const staleStart = startMs - 5 * BUCKET_MS;
  const stale = {
    start_ms: staleStart,
    connect_attempts: 0,
    connect_successes: 0,
    games_started: 0,
    games_completed: 0,
    rtt_histogram: zeros(),
    announced_players_max: 9,
  };

  const afterFirst = recordAnnouncedPlayers({ buckets: [stale] }, 3, now, BUCKET_MS, WINDOW_MS, EDGES);
  const current = afterFirst.buckets.find((b) => b.start_ms === startMs);
  assert.equal(current.announced_players_max, 3);
  // An older window's peak is a fact about that window: a writer that touched
  // every bucket would fail here.
  assert.equal(afterFirst.buckets.find((b) => b.start_ms === staleStart).announced_players_max, 9);

  // A raise, never an overwrite — a heartbeat catching an empty moment must
  // not erase the peak the window already saw.
  const afterSecond = recordAnnouncedPlayers(afterFirst, 1, now, BUCKET_MS, WINDOW_MS, EDGES);
  assert.equal(afterSecond.buckets.find((b) => b.start_ms === startMs).announced_players_max, 3);
  const afterThird = recordAnnouncedPlayers(afterSecond, 8, now, BUCKET_MS, WINDOW_MS, EDGES);
  assert.equal(afterThird.buckets.find((b) => b.start_ms === startMs).announced_players_max, 8);

  // This writer AGES OUT, and it has to: until a client reporter exists it is
  // the counters' only writer, so an append-only version grows the stored blob
  // by one bucket per announce window forever and nothing else prunes it. The
  // pair is what makes the assertion about the window rather than about
  // "drops something": one bucket just outside the window disappears, one just
  // inside it survives the same call.
  const expired = { ...stale, start_ms: startMs - (WINDOW_MS + BUCKET_MS), announced_players_max: 4 };
  const surviving = { ...stale, start_ms: startMs - (WINDOW_MS - BUCKET_MS), announced_players_max: 5 };
  const before = { buckets: [expired, surviving] };
  assert.equal(before.buckets.length, 2, "both fixtures must be present BEFORE the write");

  const aged = recordAnnouncedPlayers(before, 2, now, BUCKET_MS, WINDOW_MS, EDGES);
  assert.equal(
    aged.buckets.find((b) => b.start_ms === expired.start_ms),
    undefined,
    "a bucket past the decay window must not survive a write",
  );
  assert.equal(aged.buckets.find((b) => b.start_ms === surviving.start_ms).announced_players_max, 5);
  assert.deepEqual(
    aged.buckets.map((b) => b.start_ms).sort((a, b) => a - b),
    [surviving.start_ms, startMs],
  );

  // The producer and the guard, wired end to end: without this write the
  // guard drops every game outcome forever, while V-U13d — which injects the
  // field — stays green.
  const fold = foldMetricReports(
    sanitizeMetricsBatch(
      batch([{ url: KNOWN_URL, outcome: "game_completed", game_code: "ABC123" }]),
    ),
    new Map([[KNOWN_URL, afterFirst]]),
    now,
    BUCKET_MS,
    WINDOW_MS,
    EDGES,
  );
  assert.equal(fold.accepted.length, 1);
});

test("V-U13g: the bucket width is a parameter, not a module literal", () => {
  // The fixture clock is load-bearing: `start = now - now % bucketMs` makes a
  // full-width and a half-width bucket AGREE whenever the clock sits in the
  // first half of a bucket, so half of all clocks would make this row pass for
  // a broken implementation. The precondition is asserted, not assumed —
  // 2026-09-02T00:25:00Z is 1,500,000 ms into a 2,700,000 ms bucket.
  //
  // (Every wall-clock-round instant this test used while BUCKET_MS was an hour
  // FAILS this precondition at 45 minutes, which is why the assertion below
  // comes before the comparison rather than after it.)
  const now = Date.parse("2026-09-02T00:25:00.000Z");
  assert.ok(
    now % BUCKET_MS >= BUCKET_MS / 2,
    "the fixture clock must sit in the second half of a bucket",
  );

  const known = new Map([[KNOWN_URL, { buckets: [] }]]);
  const reports = sanitizeMetricsBatch(batch([{ url: KNOWN_URL, outcome: "connect_ok", rtt_ms: 10 }]));

  const wide = foldMetricReports(reports, known, now, BUCKET_MS, WINDOW_MS, EDGES);
  const narrow = foldMetricReports(reports, known, now, BUCKET_MS / 2, WINDOW_MS, EDGES);

  const wideStart = wide.counters.get(KNOWN_URL).buckets[0].start_ms;
  const narrowStart = narrow.counters.get(KNOWN_URL).buckets[0].start_ms;
  // Paired reach-guard: both folds produced a bucket, so "different" is not
  // two empty results.
  assert.equal(wide.accepted.length, 1);
  assert.equal(narrow.accepted.length, 1);
  assert.notEqual(wideStart, narrowStart);
  // Asserted as measured literals rather than as `now - now % BUCKET_MS`,
  // which would just restate the implementation.
  assert.equal(wideStart, 1_788_307_200_000);
  assert.equal(narrowStart, 1_788_308_550_000);
});

test("the fold ages buckets out of the decay window", () => {
  const now = 10 * WINDOW_MS + 45 * 60_000;
  const startMs = now - (now % BUCKET_MS);
  const ancient = {
    start_ms: startMs - 25 * BUCKET_MS,
    connect_attempts: 100,
    connect_successes: 100,
    games_started: 0,
    games_completed: 0,
    rtt_histogram: zeros(),
    announced_players_max: 0,
  };
  const recent = { ...ancient, start_ms: startMs - 2 * BUCKET_MS };

  const fold = foldMetricReports(
    sanitizeMetricsBatch(batch([{ url: KNOWN_URL, outcome: "connect_ok", rtt_ms: 10 }])),
    new Map([[KNOWN_URL, { buckets: [ancient, recent] }]]),
    now,
    BUCKET_MS,
    WINDOW_MS,
    EDGES,
  );
  const kept = fold.counters.get(KNOWN_URL).buckets.map((b) => b.start_ms).sort();
  assert.deepEqual(kept, [recent.start_ms, startMs].sort());
});

test("serverProbeEvents projects through the shared column layout", () => {
  const [event] = serverProbeEvents([
    { url: KNOWN_URL, outcome: "connect_ok", rtt_ms: 42, game_code: "ABC123" },
  ]);
  assert.equal(event.event, "server_probe");
  // A server probe has no client envelope; inventing one would put a value in
  // a column that means something else for every other event.
  assert.deepEqual([event.appVersion, event.buildHash, event.platform], ["", "", ""]);
  assert.deepEqual(event.blobs, [KNOWN_URL, "connect_ok", "ABC123"]);
  assert.deepEqual(event.doubles, [42]);

  const [missing] = serverProbeEvents([{ url: KNOWN_URL, outcome: "connect_fail" }]);
  assert.deepEqual(missing.blobs, [KNOWN_URL, "connect_fail", ""]);
  assert.deepEqual(missing.doubles, [0]);
});

// `fetchInfoDocument` lives in `lobby-do.ts`, which imports `DurableObject` and
// so cannot be loaded outside workerd — there is no harness here that can drive
// a real redirect at it. This asserts the option is present in the source
// instead. That is a weaker claim than a behavioural test and is written down
// as one: it proves the flag has not been dropped, not that the runtime honours
// it. It exists because the alternative is no gate at all on a verification
// fetch that any caller can aim, announcing being open by design.
test("the info-document verification fetch refuses redirects", () => {
  const source = readFileSync(
    fileURLToPath(new URL("../src/lobby-do.ts", import.meta.url)),
    "utf8",
  );
  const call = source.slice(source.indexOf("await fetch(infoUrl, {"));
  assert.ok(
    call.startsWith("await fetch(infoUrl, {"),
    "the verification fetch call was not found — this guard is keyed on it",
  );
  const options = call.slice(0, call.indexOf("});") + 3);
  assert.match(
    options,
    /redirect:\s*"error"/,
    'the verification fetch must pass redirect: "error" so a redirecting host '
      + "fails closed instead of pointing the verifier at another origin",
  );
});
