import { describe, expect, it } from "vitest";

import type { GameEvent } from "../../adapter/types";
import { normalizeEvents } from "../eventNormalizer";

describe("normalizeEvents", () => {
  it("returns empty array for empty events", () => {
    expect(normalizeEvents([])).toEqual([]);
  });

  it("skips non-visual events", () => {
    const events: GameEvent[] = [
      { type: "PriorityPassed", data: { player_id: 0 } },
      { type: "MulliganStarted" },
      { type: "GameStarted" },
      { type: "ManaAdded", data: { player_id: 0, mana_type: "White", source_id: 1 } },
      { type: "DamageCleared", data: { object_id: 1 } },
      { type: "CardsDrawn", data: { player_id: 0, count: 1 } },
      { type: "CardDrawn", data: { player_id: 0, object_id: 1 } },
      { type: "PermanentTapped", data: { object_id: 1 } },
      { type: "PermanentUntapped", data: { object_id: 1 } },
    ];

    expect(normalizeEvents(events)).toEqual([]);
  });

  it("SpellCast always starts a new step", () => {
    const events: GameEvent[] = [
      { type: "SpellCast", data: { card_id: 1, controller: 0 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects[0].type).toBe("SpellCast");
    expect(steps[0].duration).toBe(500);
  });

  it("consecutive DamageDealt events group into one step", () => {
    const events: GameEvent[] = [
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 3 } },
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 3 }, amount: 2 } },
      { type: "DamageDealt", data: { source_id: 4, target: { Player: 0 }, amount: 5 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects).toHaveLength(3);
    expect(steps[0].duration).toBe(300);
  });

  it("consecutive CreatureDestroyed events group into one step (board wipe)", () => {
    const events: GameEvent[] = [
      { type: "CreatureDestroyed", data: { object_id: 1 } },
      { type: "CreatureDestroyed", data: { object_id: 2 } },
      { type: "CreatureDestroyed", data: { object_id: 3 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects).toHaveLength(3);
    expect(steps[0].duration).toBe(400);
  });

  it("ZoneChanged groups with preceding cause (SpellCast)", () => {
    const events: GameEvent[] = [
      { type: "SpellCast", data: { card_id: 1, controller: 0 } },
      { type: "ZoneChanged", data: { object_id: 1, from: "Stack", to: "Battlefield" } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects).toHaveLength(2);
    expect(steps[0].duration).toBe(500); // max(500, 400) = 500
  });

  it("LifeChanged groups with concurrent DamageDealt step", () => {
    const events: GameEvent[] = [
      { type: "DamageDealt", data: { source_id: 1, target: { Player: 0 }, amount: 3 } },
      { type: "LifeChanged", data: { player_id: 0, amount: -3 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects).toHaveLength(2);
  });

  it("TurnStarted creates its own step", () => {
    const events: GameEvent[] = [
      { type: "TurnStarted", data: { player_id: 0, turn_number: 1 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects[0].type).toBe("TurnStarted");
  });

  it("AttackersDeclared creates one step per attacker for staggered animation", () => {
    const events: GameEvent[] = [
      { type: "AttackersDeclared", data: { attacker_ids: [1, 2], defending_player: 1 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(2);
    expect(steps[0].effects[0].type).toBe("AttackersDeclared");
    expect(steps[0].effects[0].data).toEqual({ attacker_ids: [1], defending_player: 1 });
    expect(steps[1].effects[0].data).toEqual({ attacker_ids: [2], defending_player: 1 });
    expect(steps[0].duration).toBe(300);
  });

  it("AttackersDeclared with single attacker creates one step", () => {
    const events: GameEvent[] = [
      { type: "AttackersDeclared", data: { attacker_ids: [5], defending_player: 0 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects[0].data).toEqual({ attacker_ids: [5], defending_player: 0 });
  });

  it("BlockersDeclared gets its own step", () => {
    const events: GameEvent[] = [
      { type: "BlockersDeclared", data: { assignments: [[3, 1]] } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects[0].type).toBe("BlockersDeclared");
    expect(steps[0].duration).toBe(300);
  });

  it("step duration equals max of effect durations", () => {
    // SpellCast (500) + ZoneChanged (400) => step duration = 500
    const events: GameEvent[] = [
      { type: "SpellCast", data: { card_id: 1, controller: 0 } },
      { type: "ZoneChanged", data: { object_id: 1, from: "Hand", to: "Stack" } },
    ];

    const steps = normalizeEvents(events);
    expect(steps[0].duration).toBe(500);
  });

  it("consecutive PermanentSacrificed events group into one step", () => {
    const events: GameEvent[] = [
      { type: "PermanentSacrificed", data: { object_id: 1, player_id: 0 } },
      { type: "PermanentSacrificed", data: { object_id: 2, player_id: 0 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects).toHaveLength(2);
  });

  it("handles mixed event sequence correctly", () => {
    const events: GameEvent[] = [
      { type: "PriorityPassed", data: { player_id: 0 } },
      { type: "SpellCast", data: { card_id: 1, controller: 0 } },
      { type: "ZoneChanged", data: { object_id: 1, from: "Hand", to: "Stack" } },
      { type: "PriorityPassed", data: { player_id: 1 } },
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 3 } },
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 3 }, amount: 2 } },
      { type: "LifeChanged", data: { player_id: 1, amount: -5 } },
      { type: "CreatureDestroyed", data: { object_id: 2 } },
      { type: "CreatureDestroyed", data: { object_id: 3 } },
    ];

    const steps = normalizeEvents(events);
    // Step 1: SpellCast + ZoneChanged
    // Step 2: DamageDealt x2 + LifeChanged
    // Step 3: CreatureDestroyed x2
    expect(steps).toHaveLength(3);
    expect(steps[0].effects).toHaveLength(2);
    expect(steps[1].effects).toHaveLength(3);
    expect(steps[2].effects).toHaveLength(2);
  });

  it("skips StackPushed, StackResolved, and ReplacementApplied", () => {
    const events: GameEvent[] = [
      { type: "StackPushed", data: { object_id: 1 } },
      { type: "StackResolved", data: { object_id: 1 } },
      { type: "ReplacementApplied", data: { source_id: 1, event_type: "draw" } },
    ];

    expect(normalizeEvents(events)).toEqual([]);
  });
});
