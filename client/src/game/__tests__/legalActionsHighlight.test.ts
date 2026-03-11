import { describe, it, expect, beforeEach, vi } from "vitest";
import { act } from "react";

import type { GameAction, GameState } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";

/**
 * Integration test: verifies that legal actions from the engine
 * flow through the store and can be used for per-card highlighting.
 *
 * Tests the exact data shapes serde_wasm_bindgen produces, including
 * BigInt card_ids (u64 serialized as BigInt by wasm-bindgen).
 */

function createMockState(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 2,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      {
        id: 0,
        life: 20,
        mana_pool: { mana: [], total: () => 0 },
        library: [],
        hand: [100, 101, 102],
        graveyard: [],
        exile: [],
      },
      {
        id: 1,
        life: 20,
        mana_pool: { mana: [], total: () => 0 },
        library: [],
        hand: [],
        graveyard: [],
        exile: [],
      },
    ],
    priority_player: 0,
    objects: {
      100: {
        id: 100,
        card_id: 10,
        owner: 0,
        controller: 0,
        zone: "Hand",
        tapped: false,
        face_down: false,
        flipped: false,
        transformed: false,
        damage_marked: 0,
        dealt_deathtouch_damage: false,
        attached_to: null,
        attachments: [],
        counters: {},
        name: "Forest",
        power: null,
        toughness: null,
        loyalty: null,
        card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] },
        mana_cost: { type: "NoCost" },
        keywords: [],
        abilities: [],
        trigger_definitions: [],
        replacement_definitions: [],
        static_definitions: [],

        color: [],
        base_power: null,
        base_toughness: null,
        base_keywords: [],
        base_color: [],
        timestamp: 1,
        entered_battlefield_turn: null,
      },
      101: {
        id: 101,
        card_id: 11,
        owner: 0,
        controller: 0,
        zone: "Hand",
        tapped: false,
        face_down: false,
        flipped: false,
        transformed: false,
        damage_marked: 0,
        dealt_deathtouch_damage: false,
        attached_to: null,
        attachments: [],
        counters: {},
        name: "Lightning Bolt",
        power: null,
        toughness: null,
        loyalty: null,
        card_types: { supertypes: [], core_types: ["Instant"], subtypes: [] },
        mana_cost: { type: "Cost", shards: ["Red"], generic: 0 },
        keywords: [],
        abilities: ["SVar:SpellDescription:Lightning Bolt deals 3 damage"],
        trigger_definitions: [],
        replacement_definitions: [],
        static_definitions: [],

        color: ["Red"],
        base_power: null,
        base_toughness: null,
        base_keywords: [],
        base_color: ["Red"],
        timestamp: 2,
        entered_battlefield_turn: null,
      },
      102: {
        id: 102,
        card_id: 12,
        owner: 0,
        controller: 0,
        zone: "Hand",
        tapped: false,
        face_down: false,
        flipped: false,
        transformed: false,
        damage_marked: 0,
        dealt_deathtouch_damage: false,
        attached_to: null,
        attachments: [],
        counters: {},
        name: "Suntail Hawk",
        power: 1,
        toughness: 1,
        loyalty: null,
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Bird"] },
        mana_cost: { type: "Cost", shards: ["White"], generic: 0 },
        keywords: ["Flying"],
        abilities: [],
        trigger_definitions: [],
        replacement_definitions: [],
        static_definitions: [],

        color: ["White"],
        base_power: 1,
        base_toughness: 1,
        base_keywords: ["Flying"],
        base_color: ["White"],
        timestamp: 3,
        entered_battlefield_turn: null,
      },
    },
    next_object_id: 200,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 4,
    ...overrides,
  } as unknown as GameState;
}

function createMockAdapter(state: GameState, legalActions: GameAction[]) {
  return {
    initialize: vi.fn().mockResolvedValue(undefined),
    initializeGame: vi.fn().mockResolvedValue([]),
    submitAction: vi.fn().mockResolvedValue([]),
    getState: vi.fn().mockResolvedValue(state),
    getLegalActions: vi.fn().mockResolvedValue(legalActions),
    restoreState: vi.fn(),
    dispose: vi.fn(),
  };
}

/** Mimics how serde_wasm_bindgen returns legal actions with card_id as BigInt */
function bigIntAction(action: GameAction): GameAction {
  if (action.type === "PlayLand") {
    return { type: "PlayLand", data: { card_id: BigInt(action.data.card_id) } } as unknown as GameAction;
  }
  if (action.type === "CastSpell") {
    return {
      type: "CastSpell",
      data: { card_id: BigInt(action.data.card_id), targets: action.data.targets },
    } as unknown as GameAction;
  }
  return action;
}

describe("legal actions → card highlighting pipeline", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  it("stores legal actions after initGame", async () => {
    const state = createMockState();
    const legalActions: GameAction[] = [
      { type: "PassPriority" },
      { type: "PlayLand", data: { card_id: 10 } },
    ];
    const adapter = createMockAdapter(state, legalActions);

    await act(() => useGameStore.getState().initGame("test-id", adapter));

    expect(useGameStore.getState().legalActions).toEqual(legalActions);
  });

  it("stores legal actions after dispatch", async () => {
    const state = createMockState();
    const initialActions: GameAction[] = [{ type: "PassPriority" }];
    const postDispatchActions: GameAction[] = [
      { type: "PassPriority" },
      { type: "PlayLand", data: { card_id: 10 } },
    ];
    const adapter = createMockAdapter(state, initialActions);
    adapter.getLegalActions
      .mockResolvedValueOnce(initialActions)
      .mockResolvedValueOnce(postDispatchActions);

    await act(() => useGameStore.getState().initGame("test-id", adapter));
    expect(useGameStore.getState().legalActions).toEqual(initialActions);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));
    expect(useGameStore.getState().legalActions).toEqual(postDispatchActions);
  });

  it("playable card_id matching works with Number values", () => {
    const legalActions: GameAction[] = [
      { type: "PassPriority" },
      { type: "PlayLand", data: { card_id: 10 } },
    ];

    const playableCardIds = new Set<number>();
    for (const action of legalActions) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        playableCardIds.add(
          Number((action as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.card_id),
        );
      }
    }

    // Forest (card_id: 10) should be playable
    expect(playableCardIds.has(Number(10))).toBe(true);
    // Lightning Bolt (card_id: 11) should NOT be playable (no mana)
    expect(playableCardIds.has(Number(11))).toBe(false);
  });

  it("playable card_id matching works with BigInt values from WASM", () => {
    const legalActions: GameAction[] = [
      bigIntAction({ type: "PassPriority" }),
      bigIntAction({ type: "PlayLand", data: { card_id: 10 } }),
    ];

    const playableCardIds = new Set<number>();
    for (const action of legalActions) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        playableCardIds.add(
          Number((action as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.card_id),
        );
      }
    }

    // BigInt(10) coerced via Number() should match Number(10)
    expect(playableCardIds.has(Number(10))).toBe(true);
    expect(playableCardIds.has(Number(BigInt(10)))).toBe(true);

    // And obj.card_id from game state (could be BigInt) should also match
    const objCardId = BigInt(10) as unknown as number;
    expect(playableCardIds.has(Number(objCardId))).toBe(true);
  });

  it("only lands and castable spells are highlighted, not all cards", async () => {
    const state = createMockState();
    // Only Forest (card_id 10) is playable — no mana for Bolt or Hawk
    const legalActions: GameAction[] = [
      { type: "PassPriority" },
      { type: "PlayLand", data: { card_id: 10 } },
    ];
    const adapter = createMockAdapter(state, legalActions);

    await act(() => useGameStore.getState().initGame("test-id", adapter));

    const { legalActions: stored, gameState } = useGameStore.getState();
    const playableCardIds = new Set<number>();
    for (const action of stored) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        playableCardIds.add(
          Number((action as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.card_id),
        );
      }
    }

    // Verify per-card playability
    const hand = gameState!.players[0].hand;
    const objects = gameState!.objects;

    // Forest (obj 100, card_id 10) — playable
    expect(playableCardIds.has(Number(objects[hand[0]].card_id))).toBe(true);
    // Lightning Bolt (obj 101, card_id 11) — not playable (no mana)
    expect(playableCardIds.has(Number(objects[hand[1]].card_id))).toBe(false);
    // Suntail Hawk (obj 102, card_id 12) — not playable (no mana)
    expect(playableCardIds.has(Number(objects[hand[2]].card_id))).toBe(false);
  });
});
