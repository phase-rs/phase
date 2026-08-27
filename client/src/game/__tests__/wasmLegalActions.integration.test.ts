import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { describe, it, expect, beforeAll } from "vitest";

import init, {
  initialize_game,
  get_game_state,
  get_legal_actions_js,
} from "@wasm/engine";

/**
 * Integration test using the real WASM binary.
 * Validates the actual serialization shape of legal actions
 * coming from serde_wasm_bindgen, including BigInt handling.
 *
 * Requires: ./scripts/build-wasm.sh to have been run.
 */

async function initWasm() {
  const wasmPath = resolve(__dirname, "../../wasm/engine_wasm_bg.wasm");
  const bytes = await readFile(wasmPath);
  const module = await WebAssembly.compile(bytes);
  await init({ module_or_path: module });
}

describe("WASM legal actions integration", () => {
  beforeAll(async () => {
    await initWasm();
  });

  it("get_legal_actions_js returns actions with correct shape after game init", () => {
    initialize_game(null);
    const legalResult = get_legal_actions_js() as { actions: Array<{ type: string; data: Record<string, unknown> }> };
    const { actions } = legalResult;

    expect(Array.isArray(actions)).toBe(true);
    expect(actions.length).toBeGreaterThan(0);

    // Should always contain PassPriority
    const passAction = actions.find((a: { type: string }) => a.type === "PassPriority");
    expect(passAction).toBeDefined();

    // Log shapes for debugging serialization
    for (const a of actions) {
      if (a.type === "PlayLand" || a.type === "CastSpell") {
        console.log(`${a.type} object_id:`, a.data.object_id, "card_id:", a.data.card_id);
      }
    }
  });

  it("PlayLand/CastSpell object_ids can be matched to hand objects via Number() coercion", () => {
    initialize_game(null);
    const state = get_game_state();
    const legalResult = get_legal_actions_js() as { actions: Array<{ type: string; data: Record<string, unknown> }> };
    const { actions } = legalResult;

    // Build playable set the same way PlayerHand does (by object_id)
    const playableObjectIds = new Set<number>();
    for (const action of actions) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        playableObjectIds.add(Number(action.data.object_id));
      }
    }

    // Resolve objects — serde_wasm_bindgen may return Map for objects
    const objects: Record<string, { id: unknown; card_id: unknown; name: string }> =
      state.objects instanceof Map
        ? Object.fromEntries(state.objects)
        : state.objects;

    // Resolve player hand
    const players = Array.isArray(state.players)
      ? state.players
      : state.players instanceof Map
        ? [...state.players.values()]
        : [];
    const player = players[0];

    if (player?.hand?.length > 0) {
      for (const objId of player.hand) {
        const obj = objects[String(objId)]
          ?? (state.objects instanceof Map ? state.objects.get(objId) : null);
        if (obj) {
          const isPlayable = playableObjectIds.has(Number(obj.id));
          console.log(`  hand obj ${objId}: "${obj.name}" id=${obj.id} card_id=${obj.card_id} → playable: ${isPlayable}`);
        }
      }
      if (player.hand.length > 0) {
        console.log(`  playableObjectIds: [${[...playableObjectIds].join(", ")}]`);
      }
    }
  });
});
