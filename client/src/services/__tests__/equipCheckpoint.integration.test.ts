import "fake-indexeddb/auto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { gunzipSync } from "node:zlib";
import { afterAll, beforeAll, expect, it, vi } from "vitest";
import init, {
  clear_game_state, export_game_state_json, get_legal_actions_js, initialize_game, submit_action,
} from "@wasm/engine";
import { WasmAdapter } from "../../adapter/wasm-adapter";
import type { GameAction, TrustedGameStateEnvelope } from "../../adapter/types";
import { captureTrustedCheckpoint, loadGameStrict, saveResumableGameStrict } from "../gamePersistence";

let adapter: WasmAdapter;

beforeAll(async () => {
  const bytes = await readFile(resolve(__dirname, "../../wasm/engine_wasm_bg.wasm"));
  await init({ module_or_path: await WebAssembly.compile(bytes) });
  const fixture = await readFile(resolve(__dirname, "../../../../crates/engine/tests/fixtures/integration_cards.json.gz"));
  const faces = JSON.parse(gunzipSync(fixture).toString("utf8")) as Record<string, unknown>;
  const names = ["lightning greaves", "grizzly bears", "not of this world"];
  const cards = JSON.stringify(Object.fromEntries(names.map((name) => [name, faces[name]])));
  vi.stubGlobal("Worker", undefined);
  vi.stubGlobal("fetch", async () => new Response(cards));
  adapter = new WasmAdapter();
  await adapter.initialize();
  await adapter.warmCardDatabase();
}, 300_000);

afterAll(() => {
  adapter?.dispose();
  clear_game_state();
  vi.unstubAllGlobals();
});

function snapshot(): TrustedGameStateEnvelope {
  return JSON.parse(export_game_state_json()) as TrustedGameStateEnvelope;
}

function act(action: GameAction, actor = 0) {
  expect(submit_action(actor, action).status, action.type).toBe("applied");
}

function createCard(name: string, zone: "Hand" | "Battlefield"): number {
  act({ type: "Debug", data: { type: "CreateCard", data: {
    card_name: name, owner: 0, zone, run_etb: false, nonlegendary: false, creation_kind: "Card", count: 1,
  } } });
  const state = snapshot().state;
  const ids = zone === "Hand" ? state.players[0].hand : state.battlefield;
  const matches = ids.filter((id) => state.objects[id].name === name);
  expect(matches).toHaveLength(1);
  return matches[0];
}

it.each(["legacy", "pinned"] as const)("restores a %s pending equip after saving a trusted checkpoint", async (shape) => {
  initialize_game(null, 7, null, null, 2, 0);
  act({ type: "Debug", data: { type: "SetPhase", data: { phase: "PreCombatMain", active_player: 0 } } });
  const greaves = createCard("Lightning Greaves", "Battlefield");
  const bears = createCard("Grizzly Bears", "Battlefield");
  const counterspell = createCard("Not of This World", "Hand");
  act({ type: "Debug", data: { type: "AddMana", data: { player_id: 0, mana: Array(7).fill("Colorless") } } });
  act({ type: "Equip", data: { equipment_id: greaves, target_id: bears } });

  const imported = snapshot();
  expect(imported.state.stack).toHaveLength(1);
  const kind = imported.state.stack[0].kind;
  if (kind.type !== "KeywordAction" || kind.data.action.type !== "Equip") throw new Error("Expected pending Equip");
  const pins = kind.data.action.data;
  expect(pins).toEqual({
    equipment: { object_id: greaves, incarnation: expect.any(Number) },
    target_creature: { object_id: bears, incarnation: expect.any(Number) },
  });
  if (shape === "legacy") {
    // Only the historical save payload differs. Both games announced this equip through the engine.
    Object.assign(kind.data.action, { data: { equipment_id: greaves, target_creature_id: bears } });
  }
  await adapter.restoreState(imported);

  const checkpoint = await captureTrustedCheckpoint(adapter);
  if (!checkpoint) throw new Error("Expected the local engine's trusted checkpoint");
  await saveResumableGameStrict(shape, checkpoint);
  const saved = await loadGameStrict(shape);
  if (!saved) throw new Error("Expected the IndexedDB save");
  clear_game_state();
  await adapter.restoreState(saved);

  const restored = snapshot().state;
  expect(restored.stack).toHaveLength(1);
  expect(restored.stack[0].kind).toEqual({ type: "KeywordAction", data: { action: {
    type: "Equip", data: shape === "legacy" ? { equipment: greaves, target_creature: bears } : pins,
  } } });
  // Not of This World needs a spell or ability that targets a permanent we control.
  const { actions } = get_legal_actions_js() as { actions: GameAction[] };
  expect(actions.some((a) => a.type === "CastSpell" && Number(a.data.object_id) === counterspell)).toBe(shape === "pinned");
  act({ type: "PassPriority" });
  act({ type: "PassPriority" }, 1);
  const finished = snapshot().state;
  expect(finished.stack).toHaveLength(0);
  expect(finished.objects[greaves].attached_to).toEqual(shape === "pinned" ? { type: "Object", data: bears } : null);
}, 60_000);
