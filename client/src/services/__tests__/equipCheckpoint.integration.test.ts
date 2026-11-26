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
import { readIntegrationCards } from "../../test/helpers/integrationCards";
import { captureTrustedCheckpoint, loadGameStrict, saveResumableGameStrict } from "../gamePersistence";

let adapter: WasmAdapter;

beforeAll(async () => {
  const bytes = await readFile(resolve(__dirname, "../../wasm/engine_wasm_bg.wasm"));
  await init({ module_or_path: await WebAssembly.compile(bytes) });
  const faces = await readIntegrationCards();
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

it("refuses the pre-pin writer's pending Equip before replacing the installed game", async () => {
  // Unmodified export_game_state_json() from 7e2a53879, after Lightning Greaves'
  // public Equip targeted Grizzly Bears and before either player passed priority.
  const fixture = await readFile(resolve(__dirname, "legacyPendingEquip.json.gz"));
  const legacy = JSON.parse(gunzipSync(fixture).toString("utf8")) as TrustedGameStateEnvelope;
  const [greaves, bears] = legacy.state.battlefield;
  expect(legacy.state.objects[greaves].name).toBe("Lightning Greaves");
  expect(legacy.state.objects[bears].name).toBe("Grizzly Bears");
  expect(legacy.state.stack[0].kind).toEqual({ type: "KeywordAction", data: { action: {
    type: "Equip", data: { equipment_id: greaves, target_creature_id: bears },
  } } });
  await saveResumableGameStrict("legacy", legacy);
  const saved = await loadGameStrict("legacy");
  if (!saved) throw new Error("Expected the IndexedDB save");

  initialize_game(null, 13, null, null, 2, 0);
  const installed = snapshot();
  await expect(adapter.restoreState(saved)).rejects.toContain(
    "unsupported stack object: pending Equip is missing the Equipment or target creature incarnation",
  );
  expect(snapshot()).toEqual(installed);
}, 60_000);

it("restores a pinned pending Equip from a trusted checkpoint and attaches its Equipment", async () => {
  initialize_game(null, 7, null, null, 2, 0);
  act({ type: "Debug", data: { type: "SetPhase", data: { phase: "PreCombatMain", active_player: 0 } } });
  const greaves = createCard("Lightning Greaves", "Battlefield");
  const bears = createCard("Grizzly Bears", "Battlefield");
  const counterspell = createCard("Not of This World", "Hand");
  act({ type: "Debug", data: { type: "AddMana", data: { player_id: 0, mana: Array(7).fill("Colorless") } } });
  act({ type: "Equip", data: { equipment_id: greaves, target_id: bears } });

  const announced = snapshot();
  expect(announced.state.stack).toHaveLength(1);
  const kind = announced.state.stack[0].kind;
  if (kind.type !== "KeywordAction" || kind.data.action.type !== "Equip") throw new Error("Expected pending Equip");
  const pins = kind.data.action.data;
  expect(pins).toEqual({
    equipment: { object_id: greaves, incarnation: expect.any(Number) },
    target_creature: { object_id: bears, incarnation: expect.any(Number) },
  });
  const checkpoint = await captureTrustedCheckpoint(adapter);
  if (!checkpoint) throw new Error("Expected the local engine's trusted checkpoint");
  await saveResumableGameStrict("pinned", checkpoint);
  const saved = await loadGameStrict("pinned");
  if (!saved) throw new Error("Expected the IndexedDB save");
  clear_game_state();
  await adapter.restoreState(saved);

  const restored = snapshot().state;
  expect(restored.stack).toHaveLength(1);
  expect(restored.stack[0].kind).toEqual({ type: "KeywordAction", data: { action: {
    type: "Equip", data: pins,
  } } });
  // Not of This World needs a spell or ability that targets a permanent we control.
  const { actions } = get_legal_actions_js() as { actions: GameAction[] };
  expect(actions.some((a) => a.type === "CastSpell" && Number(a.data.object_id) === counterspell)).toBe(true);
  act({ type: "PassPriority" });
  act({ type: "PassPriority" }, 1);
  const finished = snapshot().state;
  expect(finished.stack).toHaveLength(0);
  expect(finished.objects[greaves].attached_to).toEqual({ type: "Object", data: bears });
}, 60_000);
