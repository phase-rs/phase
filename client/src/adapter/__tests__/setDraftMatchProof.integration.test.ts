import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { beforeAll, describe, expect, it, vi } from "vitest";
import initDraft, { start_quick_cube_draft } from "@wasm/draft";
import initEngine, { get_game_state, initialize_game, load_card_database } from "@wasm/engine";

import fixture from "../../test/fixtures/draftMatchProof.json";
import { DraftAdapter, type DraftPlayerView, type PoolInput } from "../draft-adapter";
import { P2PDraftHost } from "../p2p-draft-host";
import type { DraftMatchLaunch } from "../../network/draftProtocol";
import { formatMetadata } from "../../data/formatRegistry";

const wasmDir = resolve(__dirname, "../../wasm");

function cardData(): string {
  const cards = Object.fromEntries([...fixture.spells, ...fixture.basics].map((name) => [
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
  ]));
  return JSON.stringify(cards);
}

function setPool(): PoolInput {
  const code = fixture.set_code;
  return {
    type: "Set",
    data: {
      pools: [{
        code,
        name: "Proof Set",
        release_date: null,
        pack_variants: [{
          contents: [{ slot: "common", count: 4, choices: [{ sheet: "common", weight: 1 }] }],
          weight: 1,
        }],
        pack_variants_total_weight: 1,
        sheets: {
          common: {
            cards: fixture.spells.map((name, index) => ({
              name, set_code: code, collector_number: String(index + 1), rarity: "common", weight: 1,
            })),
            total_weight: fixture.spells.length,
            foil: false,
            balance_colors: false,
          },
        },
        prints: [],
        basic_lands: [],
      }],
      sequence: [code],
    },
  };
}

function sorted(cards: string[]): string[] {
  return [...cards].sort();
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

describe("Set draft to real bot match", () => {
  beforeAll(async () => {
    const [draftBytes, engineBytes] = await Promise.all([
      readFile(resolve(wasmDir, "draft_wasm_bg.wasm")),
      readFile(resolve(wasmDir, "engine_wasm_bg.wasm")),
    ]);
    await initDraft({ module_or_path: await WebAssembly.compile(draftBytes) });
    await initEngine({ module_or_path: await WebAssembly.compile(engineBytes) });
    load_card_database(cardData());
  }, 300_000);

  it("starts an emitted Premier Bot launch while draft CARD_DB is absent", async () => {
    // This reaches the loaded-database guard before any draft session is installed.
    expect(() => start_quick_cube_draft("1 Forest", "absence probe", "", 0, 1))
      .toThrow("Card database must be loaded before cube draft");

    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const draftLoadSpy = vi.spyOn(DraftAdapter.prototype, "loadCardDatabase");
    const botDeckSpy = vi.spyOn(DraftAdapter.prototype, "getBotDeck");
    try {
      const source = setPool();
      const adapter = new DraftAdapter();
      const seats = [
        { type: "Human" as const, player_id: 0, display_name: "Host" },
        { type: "Bot" as const, name: "Bot 1" },
      ];
      let view = await adapter.createMultiplayerDraft(
        source, seats, "Premier", 117, "set-proof", "Swiss", "Competitive", 0,
      );
      expect(view.status).toBe("Drafting");
      for (let step = 0; step < 60 && view.status === "Drafting"; step += 1) {
        for (let seat = 0; seat < seats.length; seat += 1) {
          const seatView = await adapter.getViewForSeat(seat);
          if (seatView.current_pack?.length && seatView.seats[seat].pick_status === "Pending") {
            await adapter.submitPickForSeat(seat, [seatView.current_pack[0].instance_id]);
          }
        }
        view = await adapter.getViewForSeat(0);
      }
      expect(view.status).toBe("Deckbuilding");
      expect(view.pool.length).toBeGreaterThan(0);
      const humanDeck = [
        ...view.pool.map((card) => card.name),
        ...Array<string>(40 - view.pool.length).fill("Forest"),
      ];
      expect(humanDeck).toHaveLength(40);

      const host = new P2PDraftHost(
        { id: "host" } as never, () => () => {}, source, "Premier", 2, "Host", "Swiss", "Competitive",
      );
      const privateHost = host as unknown as {
        adapter: DraftAdapter;
        draftStarted: boolean;
        paused: boolean;
        persistSessionStrict: () => Promise<void>;
        matchLaunches: Map<string, Map<number, DraftMatchLaunch>>;
      };
      privateHost.adapter = adapter;
      privateHost.draftStarted = true;
      privateHost.paused = false;
      privateHost.persistSessionStrict = async () => {};
      const launches: DraftMatchLaunch[] = [];
      host.onEvent((event) => {
        if (event.type === "matchStart") launches.push(event.launch);
      });

      await host.submitHostDeck(humanDeck, []);
      const pairedView: DraftPlayerView = await adapter.getViewForSeat(0);
      expect(pairedView.status).toBe("MatchInProgress");
      const pairing = pairedView.pairings.find((candidate) =>
        candidate.seat_a === 0 || candidate.seat_b === 0);
      expect(pairing).toBeDefined();
      const botSeat = pairing!.seat_a === 0 ? pairing!.seat_b : pairing!.seat_a;
      const botView = await adapter.getViewForSeat(botSeat);
      expect(botView.pool.length).toBeGreaterThan(0);
      expect(botDeckSpy).toHaveBeenCalledExactlyOnceWith(botSeat);
      const suggested = await adapter.getBotDeck(botSeat);
      const proposed = [
        ...suggested.main_deck,
        ...Object.entries(suggested.lands).flatMap(([name, count]) => Array<string>(count).fill(name)),
      ];
      expect(proposed).toHaveLength(40);
      await expect(adapter.getBotDeck(99)).rejects.toThrow("bot_seat is out of range");
      expect(launches).toHaveLength(1);
      const launch = launches[0];
      expect(launch.type).toBe("Bot");
      if (launch.type !== "Bot") throw new Error("Expected Bot launch");
      expect(launch.matchId).toBe(pairing!.match_id);
      expect(launch.localSeat).toBe(0);
      expect(launch.botSeat).toBe(botSeat);
      expect(sorted(launch.deckPayload.opponent.main_deck)).toEqual(sorted(proposed));
      expect(launch.deckPayload.player.main_deck).toEqual(humanDeck);
      expect(privateHost.matchLaunches.get(pairing!.match_id)?.get(0)).toEqual(launch);
      expect(draftLoadSpy).not.toHaveBeenCalled();
      expect(fetchSpy).not.toHaveBeenCalled();

      const result = initialize_game(
        launch.deckPayload, 117, formatMetadata("Limited")!.default_config,
        launch.matchConfig, 2, 0,
      ) as { error?: boolean; reasons?: string[] };
      expect(result.error, result.reasons?.join("; ")).not.toBe(true);
      const decks = installedDecks();
      expect(decks).toHaveLength(2);
      expect(sorted(decks[0])).toEqual(sorted(humanDeck));
      expect(sorted(decks[1])).toEqual(sorted(launch.deckPayload.opponent.main_deck));
      console.info(`Set proof: CARD_DB absent; pairing ${pairing!.match_id}, bot seat ${botSeat}, bot deck ${proposed.length}, game decks ${decks.map((deck) => deck.length).join("/")}`);
    } finally {
      fetchSpy.mockRestore();
      draftLoadSpy.mockRestore();
      botDeckSpy.mockRestore();
    }
  }, 300_000);
});
