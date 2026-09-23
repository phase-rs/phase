import assert from "node:assert/strict";
import test, { beforeEach } from "node:test";

import { handleTurnCredentials } from "../src/turn.ts";

const originalFetch = globalThis.fetch;

beforeEach(() => {
  globalThis.fetch = originalFetch;
});

function turnRequest(method = "GET") {
  return new Request("https://lobby.example/turn-credentials", {
    method,
    headers: {
      Origin: "https://phase-rs.dev",
      "CF-Connecting-IP": "203.0.113.7",
    },
  });
}

function configuredEnv(limiter) {
  return {
    TURN_KEY_ID: "phase-key",
    TURN_KEY_API_TOKEN: "secret-is-only-used-by-the-mock",
    TURN_TTL_SECONDS: "300",
    ALLOWED_ORIGINS: "https://phase-rs.dev",
    TURN_LIMIT: limiter,
  };
}

test("allows a GET after the limiter and mints TURN credentials once", async () => {
  const limiterCalls = [];
  let upstreamCalls = 0;
  globalThis.fetch = async (input, init) => {
    upstreamCalls += 1;
    assert.equal(
      input,
      "https://rtc.live.cloudflare.com/v1/turn/keys/phase-key/credentials/generate-ice-servers",
    );
    assert.equal(init.method, "POST");
    assert.equal(init.headers.Authorization, "Bearer secret-is-only-used-by-the-mock");
    assert.deepEqual(JSON.parse(init.body), {
      ttl: 300,
      customIdentifier: "XX-AS0",
    });
    return new Response(JSON.stringify({ iceServers: [{ urls: "stun:example.test" }] }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };

  const response = await handleTurnCredentials(
    turnRequest(),
    configuredEnv({
      async limit(options) {
        limiterCalls.push(options);
        return { success: true };
      },
    }),
  );

  assert.equal(response.status, 200);
  assert.deepEqual(limiterCalls, [{ key: "turn:203.0.113.7" }]);
  assert.equal(upstreamCalls, 1);
  assert.deepEqual(await response.json(), { iceServers: [{ urls: "stun:example.test" }] });
});

test("returns 429 and skips the TURN API when the limiter refuses", async () => {
  let upstreamCalls = 0;
  globalThis.fetch = async () => {
    upstreamCalls += 1;
    throw new Error("the upstream must not be reached");
  };

  const response = await handleTurnCredentials(
    turnRequest(),
    configuredEnv({ async limit() { return { success: false }; } }),
  );

  assert.equal(response.status, 429);
  assert.equal(response.headers.get("Access-Control-Allow-Origin"), "https://phase-rs.dev");
  assert.deepEqual(await response.json(), { error: "rate_limited" });
  assert.equal(upstreamCalls, 0);
});

test("returns 503 and skips the TURN API when the limiter fails", async () => {
  let upstreamCalls = 0;
  globalThis.fetch = async () => {
    upstreamCalls += 1;
    throw new Error("the upstream must not be reached");
  };

  const response = await handleTurnCredentials(
    turnRequest(),
    configuredEnv({
      async limit() {
        throw new Error("rate limiter unavailable");
      },
    }),
  );

  assert.equal(response.status, 503);
  assert.equal(response.headers.get("Access-Control-Allow-Origin"), "https://phase-rs.dev");
  assert.deepEqual(await response.json(), { error: "TURN rate limiter unavailable" });
  assert.equal(upstreamCalls, 0);
});

test("returns 503 and skips the TURN API when the limiter binding is missing", async () => {
  let upstreamCalls = 0;
  globalThis.fetch = async () => {
    upstreamCalls += 1;
    throw new Error("the upstream must not be reached");
  };

  const env = configuredEnv(undefined);
  const response = await handleTurnCredentials(turnRequest(), env);

  assert.equal(response.status, 503);
  assert.equal(response.headers.get("Access-Control-Allow-Origin"), "https://phase-rs.dev");
  assert.deepEqual(await response.json(), { error: "TURN rate limiter unavailable" });
  assert.equal(upstreamCalls, 0);
});

test("OPTIONS does not spend a limiter budget or call the TURN API", async () => {
  let limiterCalls = 0;
  let upstreamCalls = 0;
  globalThis.fetch = async () => {
    upstreamCalls += 1;
    throw new Error("the upstream must not be reached");
  };

  const response = await handleTurnCredentials(
    turnRequest("OPTIONS"),
    configuredEnv({
      async limit() {
        limiterCalls += 1;
        return { success: false };
      },
    }),
  );

  assert.equal(response.status, 204);
  assert.equal(limiterCalls, 0);
  assert.equal(upstreamCalls, 0);
});
