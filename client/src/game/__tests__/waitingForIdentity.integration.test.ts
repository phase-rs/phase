import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { afterEach, beforeAll, expect, it } from "vitest";

import init, { clear_game_state, get_game_state, initialize_game, submit_action } from "@wasm/engine";
import type { DerivedViews, GameState } from "../../adapter/types";

beforeAll(async () => {
  const bytes = await readFile(resolve(__dirname, "../../wasm/engine_wasm_bg.wasm"));
  await init({ module_or_path: await WebAssembly.compile(bytes) });
});

afterEach(() => clear_game_state());

it("W1/W2 publishes the native Priority digest and advances the delivered action revision", () => {
  clear_game_state();
  initialize_game(null, 42, null, null, 2, 0);
  const before = get_game_state() as { state: GameState; derived: DerivedViews };
  expect(before.state.waiting_for).toEqual({ type: "Priority", data: { player: 0 } });
  expect(before.derived.waiting_for_identity).toBe("ae290362dc64d95b65ecb96ab3bd73cb33c820ddf926c06d687a9631c217bd1d");
  expect(typeof before.derived.state_revision).toBe("number");

  const result = submit_action(0, { type: "PassPriority" });
  expect(result.status).toBe("applied");
  const after = get_game_state() as { state: GameState; derived: DerivedViews };
  expect(after.state.waiting_for).toEqual({ type: "Priority", data: { player: 1 } });
  expect(typeof after.derived.waiting_for_identity).toBe("string");
  expect(after.derived.state_revision).toBeGreaterThan(before.derived.state_revision!);
});
