import assert from "node:assert/strict";
import { test } from "node:test";

import {
  buildStatsPayload,
  countGameOutbounds,
  dayBucket,
  DAILY_PREFIX,
} from "../src/stats.ts";

test("countGameOutbounds counts GameCreated and PeerInfo, ignores the rest", () => {
  const outbounds = [
    { kind: "ToSelf", msg: { type: "GameCreated" } },
    { kind: "ToSelf", msg: { type: "PeerInfo" } },
    { kind: "ToSelf", msg: { type: "GameCreated" } },
    // Same message types on a non-ToSelf scope must not count.
    { kind: "ToSubscribers", msg: { type: "GameCreated" } },
    { kind: "ToSubscribers", msg: { type: "PeerInfo" } },
    // Other ToSelf message types are ignored.
    { kind: "ToSelf", msg: { type: "LobbyUpdate" } },
    { kind: "ToSelf" },
    { kind: "SendPlayerCountToSelf" },
  ];
  assert.deepEqual(countGameOutbounds(outbounds), { created: 2, joined: 1 });
});

test("countGameOutbounds returns zero for an empty batch", () => {
  assert.deepEqual(countGameOutbounds([]), { created: 0, joined: 0 });
});

test("countGameOutbounds sums multiple in one batch", () => {
  const outbounds = [
    { kind: "ToSelf", msg: { type: "PeerInfo" } },
    { kind: "ToSelf", msg: { type: "PeerInfo" } },
    { kind: "ToSelf", msg: { type: "GameCreated" } },
  ];
  assert.deepEqual(countGameOutbounds(outbounds), { created: 1, joined: 2 });
});

test("dayBucket returns the UTC YYYY-MM-DD for a fixed epoch", () => {
  // 2026-07-03T18:04:55Z
  assert.equal(dayBucket(Date.parse("2026-07-03T18:04:55.000Z")), "2026-07-03");
  // Just before midnight UTC stays on the same day; the ms are truncated.
  assert.equal(dayBucket(Date.parse("2026-07-03T23:59:59.999Z")), "2026-07-03");
  // One ms later crosses into the next UTC day.
  assert.equal(dayBucket(Date.parse("2026-07-04T00:00:00.000Z")), "2026-07-04");
});

test("buildStatsPayload passes durable scalars and live gauges through", () => {
  const payload = buildStatsPayload({
    playersOnline: 7,
    playersPeak: 42,
    activeGames: 3,
    gamesCreatedTotal: 100,
    gamesJoinedTotal: 80,
    daily: new Map(),
    nowMs: Date.parse("2026-07-03T12:00:00.000Z"),
  });
  assert.equal(payload.players_online, 7);
  assert.equal(payload.players_peak, 42);
  assert.equal(payload.active_games, 3);
  assert.equal(payload.games_created_total, 100);
  assert.equal(payload.games_joined_total, 80);
  // No buckets at all → today derives to zero and the series is empty.
  assert.equal(payload.games_created_today, 0);
  assert.equal(payload.games_joined_today, 0);
  assert.deepEqual(payload.daily, []);
});

test("buildStatsPayload derives today from the injected clock and orders chronologically", () => {
  // Storage lists reverse (newest-first); keys carry the full prefix.
  const daily = new Map([
    [`${DAILY_PREFIX}2026-07-03`, { created: 5, joined: 4 }],
    [`${DAILY_PREFIX}2026-07-02`, { created: 2, joined: 1 }],
    [`${DAILY_PREFIX}2026-07-01`, { created: 9, joined: 8 }],
  ]);
  const payload = buildStatsPayload({
    playersOnline: 0,
    playersPeak: 0,
    activeGames: 0,
    gamesCreatedTotal: 16,
    gamesJoinedTotal: 13,
    daily,
    nowMs: Date.parse("2026-07-03T09:30:00.000Z"),
  });
  // Series flipped to chronological (oldest-first), prefix stripped.
  assert.deepEqual(payload.daily, [
    { date: "2026-07-01", created: 9, joined: 8 },
    { date: "2026-07-02", created: 2, joined: 1 },
    { date: "2026-07-03", created: 5, joined: 4 },
  ]);
  // Today lookup uses nowMs, not the wall clock.
  assert.equal(payload.games_created_today, 5);
  assert.equal(payload.games_joined_today, 4);
});

test("buildStatsPayload reports zero for today when the bucket is missing", () => {
  const daily = new Map([
    [`${DAILY_PREFIX}2026-07-01`, { created: 3, joined: 2 }],
  ]);
  const payload = buildStatsPayload({
    playersOnline: 0,
    playersPeak: 0,
    activeGames: 0,
    gamesCreatedTotal: 3,
    gamesJoinedTotal: 2,
    daily,
    nowMs: Date.parse("2026-07-05T00:00:00.000Z"),
  });
  assert.equal(payload.games_created_today, 0);
  assert.equal(payload.games_joined_today, 0);
});

test("buildStatsPayload caps the series at 30 newest days", () => {
  // 40 days, newest-first as storage would list them.
  const entries = [];
  for (let day = 40; day >= 1; day--) {
    const date = `2026-01-${String(day).padStart(2, "0")}`;
    entries.push([`${DAILY_PREFIX}${date}`, { created: day, joined: day }]);
  }
  const payload = buildStatsPayload({
    playersOnline: 0,
    playersPeak: 0,
    activeGames: 0,
    gamesCreatedTotal: 0,
    gamesJoinedTotal: 0,
    daily: new Map(entries),
    nowMs: Date.parse("2026-02-15T00:00:00.000Z"),
  });
  assert.equal(payload.daily.length, 30);
  // Kept the newest 30 (days 11..40), oldest-first after the flip.
  assert.equal(payload.daily[0].date, "2026-01-11");
  assert.equal(payload.daily[29].date, "2026-01-40");
});
