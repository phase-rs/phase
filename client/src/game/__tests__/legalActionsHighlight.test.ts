import { describe, it, expect, beforeEach, vi } from "vitest";
import { act } from "react";

import type { GameAction, GameState } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";

/**
 * Integration test: verifies that legal actions from the engine
 * flow through the store and can be used for per-card highlighting.
 *
 * Tests the exact data shapes serde_wasm_bindgen produces, including
 * BigInt object_ids/card_ids (u64 serialized as BigInt by wasm-bindgen).
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
        card_types: {
          core_types: ["Land"],
          subtypes: ["Forest"],
          supertypes: [],
        },
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
        card_types: {
          core_types: ["Instant"],
          subtypes: [],
          supertypes: [],
        },
        mana_cost: { type: "Cost", shards: ["Red"], generic: 0 },
        keywords: [],
        abilities: [],
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
        card_types: {
          core_types: ["Creature"],
          subtypes: ["Bird"],
          supertypes: [],
        },
        mana_cost: { type: "Cost", shards: ["White"], generic: 0 },
        keywords: [],
        abilities: [],
        trigger_definitions: [],
        replacement_definitions: [],
        static_definitions: [],
        color: ["White"],
        base_power: 1,
        base_toughness: 1,
        base_keywords: [],
        base_color: ["White"],
        timestamp: 3,
        entered_battlefield_turn: null,
      },
    },
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
    has_pending_cast: false,
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
    initializeGame: vi.fn().mockResolvedValue({ events: [] }),
    submitAction: vi.fn().mockResolvedValue({ events: [] }),
    getState: vi.fn().mockResolvedValue(state),
    getLegalActions: vi.fn().mockResolvedValue({ actions: legalActions, autoPassRecommended: false }),
    restoreState: vi.fn(),
    getAiAction: vi.fn().mockReturnValue(null),
    dispose: vi.fn(),
    estimateBracket: vi.fn().mockResolvedValue(null),
  };
}

/** Mimics how serde_wasm_bindgen returns legal actions with u64 fields as BigInt */
function bigIntAction(action: GameAction): GameAction {
  if (action.type === "PlayLand") {
    return {
      type: "PlayLand",
      data: { object_id: BigInt(action.data.object_id), card_id: BigInt(action.data.card_id) },
    } as unknown as GameAction;
  }
  if (action.type === "CastSpell") {
    return {
      type: "CastSpell",
      data: {
        object_id: BigInt(action.data.object_id),
        card_id: BigInt(action.data.card_id),
        targets: action.data.targets,
      },
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
      { type: "PlayLand", data: { object_id: 100, card_id: 10 } },
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
      { type: "PlayLand", data: { object_id: 100, card_id: 10 } },
    ];
    const adapter = createMockAdapter(state, initialActions);
    adapter.getLegalActions
      .mockResolvedValueOnce({ actions: initialActions, autoPassRecommended: false })
      .mockResolvedValueOnce({ actions: postDispatchActions, autoPassRecommended: false });

    await act(() => useGameStore.getState().initGame("test-id", adapter));
    expect(useGameStore.getState().legalActions).toEqual(initialActions);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));
    expect(useGameStore.getState().legalActions).toEqual(postDispatchActions);
  });

  it("playable object_id matching works with Number values", () => {
    const legalActions: GameAction[] = [
      { type: "PassPriority" },
      { type: "PlayLand", data: { object_id: 100, card_id: 10 } },
    ];

    const playableObjectIds = new Set<number>();
    for (const action of legalActions) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        playableObjectIds.add(
          Number((action as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.object_id),
        );
      }
    }

    // Forest (object_id: 100) should be playable
    expect(playableObjectIds.has(100)).toBe(true);
    // Lightning Bolt (object_id: 101) should NOT be playable (no mana)
    expect(playableObjectIds.has(101)).toBe(false);
  });

  it("playable object_id matching works with BigInt values from WASM", () => {
    const legalActions: GameAction[] = [
      bigIntAction({ type: "PassPriority" }),
      bigIntAction({ type: "PlayLand", data: { object_id: 100, card_id: 10 } }),
    ];

    const playableObjectIds = new Set<number>();
    for (const action of legalActions) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        playableObjectIds.add(
          Number((action as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.object_id),
        );
      }
    }

    // BigInt(100) coerced via Number() should match Number(100)
    expect(playableObjectIds.has(100)).toBe(true);
    expect(playableObjectIds.has(Number(BigInt(100)))).toBe(true);

    // And obj.id from game state (could be BigInt) should also match
    const objId = BigInt(100) as unknown as number;
    expect(playableObjectIds.has(Number(objId))).toBe(true);
  });

  it("only lands and castable spells are highlighted, not all cards", async () => {
    const state = createMockState();
    // Only Forest (object_id 100) is playable — no mana for Bolt or Hawk
    const legalActions: GameAction[] = [
      { type: "PassPriority" },
      { type: "PlayLand", data: { object_id: 100, card_id: 10 } },
    ];
    const adapter = createMockAdapter(state, legalActions);

    await act(() => useGameStore.getState().initGame("test-id", adapter));

    const { legalActions: stored, gameState } = useGameStore.getState();
    const playableObjectIds = new Set<number>();
    for (const action of stored) {
      if (action.type === "PlayLand" || action.type === "CastSpell") {
        playableObjectIds.add(
          Number((action as Extract<GameAction, { type: "PlayLand" | "CastSpell" }>).data.object_id),
        );
      }
    }

    // Verify per-card playability
    const hand = gameState!.players[0].hand;
    const objects = gameState!.objects;

    // Forest (obj 100) — playable
    expect(playableObjectIds.has(Number(objects[hand[0]].id))).toBe(true);
    // Lightning Bolt (obj 101) — not playable (no mana)
    expect(playableObjectIds.has(Number(objects[hand[1]].id))).toBe(false);
    // Suntail Hawk (obj 102) — not playable (no mana)
    expect(playableObjectIds.has(Number(objects[hand[2]].id))).toBe(false);
  });
});
