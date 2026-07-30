import { describe, it, expect, beforeEach, vi } from "vitest";
import { act } from "react";

import type { GameAction, GameState } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import { buildGameObject, buildObjectMap } from "../../test/factories/gameObjectFactory";
import { buildGameState, buildPlayer } from "../../test/factories/gameStateFactory";

/**
 * Integration test: verifies that legal actions from the engine
 * flow through the store and can be used for per-card highlighting.
 *
 * Tests the exact data shapes serde_wasm_bindgen produces, including
 * BigInt object_ids/card_ids (u64 serialized as BigInt by wasm-bindgen).
 */

function createMockState(overrides: Partial<GameState> = {}): GameState {
  const forest = buildGameObject({
    id: 100,
    card_id: 10,
    zone: "Hand",
    name: "Forest",
    card_types: {
      core_types: ["Land"],
      subtypes: ["Forest"],
      supertypes: [],
    },
    mana_cost: { type: "NoCost" },
    timestamp: 1,
  });
  const lightningBolt = buildGameObject({
    id: 101,
    card_id: 11,
    zone: "Hand",
    name: "Lightning Bolt",
    card_types: {
      core_types: ["Instant"],
      subtypes: [],
      supertypes: [],
    },
    mana_cost: { type: "Cost", shards: ["Red"], generic: 0 },
    color: ["Red"],
    base_color: ["Red"],
    timestamp: 2,
  });
  const suntailHawk = buildGameObject({
    id: 102,
    card_id: 12,
    zone: "Hand",
    name: "Suntail Hawk",
    power: 1,
    toughness: 1,
    card_types: {
      core_types: ["Creature"],
      subtypes: ["Bird"],
      supertypes: [],
    },
    mana_cost: { type: "Cost", shards: ["White"], generic: 0 },
    color: ["White"],
    base_power: 1,
    base_toughness: 1,
    base_color: ["White"],
    timestamp: 3,
  });

  return buildGameState({
    turn_number: 2,
    players: [
      buildPlayer({ id: 0, hand: [100, 101, 102] }),
      buildPlayer({ id: 1 }),
    ],
    objects: buildObjectMap(forest, lightningBolt, suntailHawk),
    rng_seed: 42,
    next_timestamp: 4,
    ...overrides,
  });
}

function createMockAdapter(state: GameState, legalActions: GameAction[]) {
  return buildEngineAdapterMock(state, {
    getLegalActions: vi.fn().mockResolvedValue({ actions: legalActions, autoPassRecommended: false }),
  });
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
