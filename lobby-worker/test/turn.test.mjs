import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import { handleTurnCredentials } from "../src/turn.ts";

const originalFetch = globalThis.fetch;

beforeEach(() => {
  globalThis.fetch = originalFetch;
});

function request(ip = "203.0.113.7") {
  return new Request("https://onedeck.example/turn-credentials", {
    headers: {
      Origin: "https://onedeck-play.pages.dev",
      "CF-Connecting-IP": ip,
    },
  });
}

function env(limiter) {
  return {
    TURN_KEY_ID: "turn-key",
    TURN_KEY_API_TOKEN: "turn-token",
    TURN_LIMIT: limiter,
    ALLOWED_ORIGINS: "https://onedeck-play.pages.dev",
  };
}

test("TURN mint is admitted by the per-IP limiter and forwards upstream", async () => {
  const keys = [];
  globalThis.fetch = async () =>
    new Response(JSON.stringify({ iceServers: [{ urls: "turn:turn.example" }] }), { status: 200 });

  const response = await handleTurnCredentials(request(), {
    ...env({
      async limit(options) {
        keys.push(options.key);
        return { success: true };
      },
    }),
  });

  assert.equal(response.status, 200);
  assert.deepEqual(keys, ["turn:203.0.113.7"]);
});

test("TURN mint is refused before upstream when the per-IP limiter rejects", async () => {
  let upstreamCalls = 0;
  globalThis.fetch = async () => {
    upstreamCalls += 1;
    return new Response("unexpected", { status: 500 });
  };

  const response = await handleTurnCredentials(
    request(),
    env({ async limit() { return { success: false }; } }),
  );

  assert.equal(response.status, 429);
  assert.equal(response.headers.get("Retry-After"), "60");
  assert.equal(upstreamCalls, 0);
});
