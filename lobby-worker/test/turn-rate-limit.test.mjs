import assert from "node:assert/strict";
import { test } from "node:test";

import {
  createRateLimiter,
  parseTurnRateLimitPerHour,
} from "../src/turn-rate-limit.ts";

test("parseTurnRateLimitPerHour clamps to [1, 120]", () => {
  assert.equal(parseTurnRateLimitPerHour(undefined), 30);
  assert.equal(parseTurnRateLimitPerHour("0"), 1);
  assert.equal(parseTurnRateLimitPerHour("999"), 120);
  assert.equal(parseTurnRateLimitPerHour("not-a-number"), 30);
});

test("createRateLimiter allows up to maxRequests per window", () => {
  const limiter = createRateLimiter({ maxRequests: 2, windowMs: 60_000 });
  const key = "1.2.3.4";
  assert.deepEqual(limiter.check(key, 1_000), { allowed: true });
  assert.deepEqual(limiter.check(key, 2_000), { allowed: true });
  const blocked = limiter.check(key, 3_000);
  assert.equal(blocked.allowed, false);
  assert.equal(blocked.retryAfterSeconds, 58);
});

test("createRateLimiter resets after the window elapses", () => {
  const limiter = createRateLimiter({ maxRequests: 1, windowMs: 1_000 });
  const key = "client";
  assert.deepEqual(limiter.check(key, 0), { allowed: true });
  assert.equal(limiter.check(key, 100).allowed, false);
  assert.deepEqual(limiter.check(key, 1_001), { allowed: true });
});
