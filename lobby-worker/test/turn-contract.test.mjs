import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const repoRoot = new URL("../../", import.meta.url);

function readRepoFile(relativePath) {
  return readFileSync(new URL(relativePath, repoRoot), "utf8");
}

test("keeps production and preview TURN limit bindings aligned with runtime", () => {
  const wranglerConfig = readRepoFile("lobby-worker/wrangler.toml");
  const turnRuntime = readRepoFile("lobby-worker/src/turn.ts");

  assert.match(
    wranglerConfig,
    /\[\[ratelimits\]\]\s+name = "TURN_LIMIT"\s+namespace_id = "1005"\s+simple = \{ limit = 30, period = 60 \}/m,
  );
  assert.match(
    wranglerConfig,
    /\[\[env\.preview\.ratelimits\]\]\s+name = "TURN_LIMIT"\s+namespace_id = "1006"\s+simple = \{ limit = 30, period = 60 \}/m,
  );
  assert.match(turnRuntime, /TURN_LIMIT\?: RateLimit/);
  assert.match(turnRuntime, /env\.TURN_LIMIT/);
});
