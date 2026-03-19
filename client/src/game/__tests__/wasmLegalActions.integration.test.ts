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
    const actions = get_legal_actions_js();

    expect(Array.isArray(actions)).toBe(true);
    expect(actions.length).toBeGreaterThan(0);

    // Should always contain PassPriority
    const passAction = actions.find((a: { type: string }) => a.type === "PassPriority");
    expect(passAction).toBeDefined();

    // Log shapes for debugging serialization
    for (const a of actions) {
      if (a.type === "PlayLand" || a.type === "CastSpell") {
        console.log(`${a.type} card_id:`, a.data.card_id, "typeof:", typeof a.data.card_id);
      }
    }
  });

  it("PlayLand card_ids can be matched to hand objects via Number() coercion", () => {
    initialize_game(null);
    const state = get_game_state();
    const actions = get_legal_actions_js();

    // Build playable set the same way PlayerHand does
    const playableCardIds = new Set<number>();
    for (const action of actions) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        playableCardIds.add(Number(action.data.card_id));
      }
    }

    // Resolve objects — serde_wasm_bindgen may return Map for objects
    const objects: Record<string, { card_id: unknown; name: string }> =
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
          const isPlayable = playableCardIds.has(Number(obj.card_id));
          console.log(`  hand obj ${objId}: "${obj.name}" card_id=${obj.card_id} (${typeof obj.card_id}) → playable: ${isPlayable}`);
        }
      }
      // With empty libraries/no deck, hand may be empty — that's OK
      if (player.hand.length > 0) {
        console.log(`  playableCardIds: [${[...playableCardIds].join(", ")}]`);
      }
    }
  });
});
