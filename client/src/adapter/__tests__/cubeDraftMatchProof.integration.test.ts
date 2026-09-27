import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { beforeAll, describe, expect, it } from "vitest";
import initDraft from "@wasm/draft";
import initEngine, {
  get_game_state, get_legal_actions_js, initialize_game, load_card_database as loadEngineCards,
} from "@wasm/engine";

import fixture from "../../test/fixtures/draftMatchProof.json";
import { DraftAdapter, type CubeDraftSettings, type DraftPlayerView } from "../draft-adapter";
import { formatMetadata } from "../../data/formatRegistry";

const wasmDir = resolve(__dirname, "../../wasm");
const cubeList = fixture.spells.map((name) => `15 ${name}`).join("\n");

function cardData(): string {
  return JSON.stringify(Object.fromEntries([...fixture.spells, ...fixture.basics].map((name) => [
    name.toLowerCase(),
    {
      name,
      mana_cost: { type: "NoCost" },
      card_type: {
        supertypes: fixture.basics.includes(name) ? ["Basic"] : [],
        core_types: [fixture.basics.includes(name) ? "Land" : "Creature"],
        subtypes: [],
      },
      power: fixture.basics.includes(name) ? null : "1",
      toughness: fixture.basics.includes(name) ? null : "1",
      loyalty: null,
      defense: null,
      oracle_text: null,
      abilities: [],
      triggers: [],
      static_abilities: [],
      replacements: [],
      keywords: [],
      bracket_signals: {
        game_changer: false, mass_land_denial: false, extra_turn: false, efficient_tutor: false,
      },
    },
  ])));
}

function settings(minDeckSize: number): CubeDraftSettings {
  return {
    pod_size: 2,
    pack_count: 2,
    cards_per_pack: 10,
    min_deck_size: minDeckSize,
    addable_cards: { policy: "StandardBasics", custom: [] },
  };
}

async function draftTwenty(adapter: DraftAdapter, minimum: number): Promise<DraftPlayerView> {
  let view = await adapter.initializeCube(cubeList, "Twenty-card proof", settings(minimum), 0, 291);
  expect(view.status).toBe("Drafting");
  for (let step = 0; step < 25 && view.status === "Drafting"; step += 1) {
    const card = view.current_pack?.[0];
    if (!card) throw new Error(`Cube draft stalled at pick ${step}: ${view.status}`);
    view = await adapter.submitPick(card.instance_id);
  }
  expect(view.status).toBe("Deckbuilding");
  expect(view.pool).toHaveLength(20);
  return view;
}

function installedDecks(): string[][] {
  const envelope = get_game_state() as Record<string, unknown>;
  const state = (envelope.state ?? envelope) as Record<string, unknown>;
  const players = state.players as Array<{ library: unknown[]; hand: unknown[] }>;
  const objects = state.objects as Record<string, { name: string }> | Map<unknown, { name: string }>;
  return players.map(({ library, hand }) => [...library, ...hand].map((id) => {
    const card = objects instanceof Map ? objects.get(id) : objects[String(id)];
    if (!card) throw new Error(`Missing installed card object ${String(id)}`);
    return card.name;
  }));
}

describe("custom Cube submission to real Limited game", () => {
  beforeAll(async () => {
    const [draftBytes, engineBytes] = await Promise.all([
      readFile(resolve(wasmDir, "draft_wasm_bg.wasm")),
      readFile(resolve(wasmDir, "engine_wasm_bg.wasm")),
    ]);
    await initDraft({ module_or_path: await WebAssembly.compile(draftBytes) });
    await initEngine({ module_or_path: await WebAssembly.compile(engineBytes) });
    const data = cardData();
    const adapter = new DraftAdapter();
    expect(await adapter.loadCardDatabase(data)).toBeGreaterThan(0);
    loadEngineCards(data);
  }, 300_000);

  it("installs the exact exported 20-card submission as an engine game", async () => {
    const adapter = new DraftAdapter();
    const view = await draftTwenty(adapter, 20);
    expect(view.min_deck_size).toBe(20);
    const submitted = view.pool.map((card) => card.name);
    expect(submitted).toHaveLength(20);
    const accepted = await adapter.submitDeck(submitted, []);
    expect(accepted.seats[0].has_submitted_deck).toBe(true);
    const session = JSON.parse(await adapter.exportSession()) as {
      submitted_decks: Record<string, { seat: number; main_deck: string[] }>;
    };
    const exported = Object.values(session.submitted_decks).find((deck) => deck.seat === 0);
    expect(exported?.main_deck).toEqual(submitted);
    if (!exported) throw new Error("Accepted Cube deck was not exported");

    const payload = {
      player: { main_deck: exported.main_deck, sideboard: [], commander: [] },
      opponent: { main_deck: Array<string>(20).fill("Forest"), sideboard: [], commander: [] },
      ai_decks: [],
    };
    expect(payload.player.main_deck).toBe(exported.main_deck);
    const result = initialize_game(
      payload, 291, formatMetadata("Limited")!.default_config, { match_type: "Bo1" }, 2, 0,
    ) as { error?: boolean; reasons?: string[] };
    expect(result.error, result.reasons?.join("; ")).not.toBe(true);
    const decks = installedDecks();
    expect(decks).toHaveLength(2);
    expect([...decks[0]].sort()).toEqual([...exported.main_deck].sort());
    expect(decks[1]).toHaveLength(20);
    expect((get_legal_actions_js() as { actions: unknown[] }).actions.length).toBeGreaterThan(0);
    console.info(`Cube proof: exported submission ${exported.main_deck.length}, game decks ${decks.map((deck) => deck.length).join("/")}`);

    const invalid = initialize_game(
      { ...payload, player: { ...payload.player, main_deck: [...exported.main_deck.slice(1), "Unknown proof card"] } },
      291, formatMetadata("Limited")!.default_config, { match_type: "Bo1" }, 2, 0,
    ) as { error?: boolean; reasons?: string[] };
    expect(invalid.error).toBe(true);
    expect(invalid.reasons?.join("; ")).toContain("Unknown proof card");
  }, 300_000);

  it("rejects the adjacent 21-card Cube minimum after reaching Deckbuilding", async () => {
    const adapter = new DraftAdapter();
    const view = await draftTwenty(adapter, 21);
    expect(view.min_deck_size).toBe(21);
    await expect(adapter.submitDeck(view.pool.map((card) => card.name), []))
      .rejects.toThrow("deck has 20 cards, minimum is 21");
  }, 300_000);
});
