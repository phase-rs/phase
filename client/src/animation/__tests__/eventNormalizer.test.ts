import { describe, expect, it } from "vitest";

import type { GameEvent } from "../../adapter/types";
import { normalizeEvents } from "../eventNormalizer";
import { EVENT_DURATIONS } from "../types";

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

  it("AttackersDeclared is non-visual and produces no steps", () => {
    const events: GameEvent[] = [
      { type: "AttackersDeclared", data: { attacker_ids: [1, 2], defending_player: 1 } },
    ];

    expect(normalizeEvents(events)).toEqual([]);
  });

  it("SpellCast always starts a new step", () => {
    const events: GameEvent[] = [
      { type: "SpellCast", data: { card_id: 1, controller: 0, object_id: 1 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects[0].event.type).toBe("SpellCast");
    expect(steps[0].duration).toBe(500);
  });

  it("DamageDealt: attacker and its blockers fight simultaneously (engagement grouping)", () => {
    // Attacker 1 hits blocker 2; blocker 2 hits attacker 1 back
    const events: GameEvent[] = [
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 3 } },
      { type: "DamageDealt", data: { source_id: 2, target: { Object: 1 }, amount: 2 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects).toHaveLength(2);
  });

  it("DamageDealt: each attacker's engagement is a separate step", () => {
    // Attacker 1 hits blocker 2; unrelated attacker 4 hits player
    const events: GameEvent[] = [
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 3 } },
      { type: "DamageDealt", data: { source_id: 4, target: { Player: 0 }, amount: 5 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(2);
    expect(steps[0].effects[0].event.type).toBe("DamageDealt");
    expect(steps[1].effects[0].event.type).toBe("DamageDealt");
  });

  it("DamageDealt: each blocker in a cluster gets its own sequential step", () => {
    // Attacker 1 deals damage to blockers 2 and 3 — each blocker fight is separate.
    // Bidirectional pairing only groups A↔B1 and A↔B2; two unidirectional hits from
    // the same attacker are distinct engagements.
    const events: GameEvent[] = [
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 2 } },
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 3 }, amount: 1 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(2);
    expect(steps[0].effects).toHaveLength(1);
    expect(steps[1].effects).toHaveLength(1);
  });

  it("DamageDealt: engine emission order (attackers then blockers) produces correct pair steps", () => {
    // Engine emits all attacker assignments before blocker assignments.
    // Expected: step 1 = {1→2, 2→1}, step 2 = {1→3, 3→1}
    const events: GameEvent[] = [
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 2 } },
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 3 }, amount: 1 } },
      { type: "DamageDealt", data: { source_id: 2, target: { Object: 1 }, amount: 2 } },
      { type: "DamageDealt", data: { source_id: 3, target: { Object: 1 }, amount: 1 } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(2);
    expect(steps[0].effects).toHaveLength(2); // 1→2 and 2→1
    expect(steps[1].effects).toHaveLength(2); // 1→3 and 3→1
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
      { type: "SpellCast", data: { card_id: 1, controller: 0, object_id: 1 } },
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
    expect(steps[0].effects[0].event.type).toBe("TurnStarted");
  });

  it("BlockersDeclared gets its own step", () => {
    const events: GameEvent[] = [
      { type: "BlockersDeclared", data: { assignments: [[3, 1]] } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects[0].event.type).toBe("BlockersDeclared");
    expect(steps[0].duration).toBe(EVENT_DURATIONS.BlockersDeclared);
  });

  it("combat pacing scales combat-only step durations", () => {
    const events: GameEvent[] = [
      { type: "BlockersDeclared", data: { assignments: [[3, 1]] } },
      { type: "DamageDealt", data: { source_id: 1, target: { Player: 0 }, amount: 3 } },
      { type: "SpellCast", data: { card_id: 9, controller: 0, object_id: 9 } },
    ];

    const steps = normalizeEvents(events, { combatPacing: "cinematic" });
    expect(steps).toHaveLength(3);
    expect(steps[0].duration).toBeGreaterThan(EVENT_DURATIONS.BlockersDeclared);
    expect(steps[1].duration).toBeGreaterThan(EVENT_DURATIONS.DamageDealt);
    expect(steps[2].duration).toBe(EVENT_DURATIONS.SpellCast);
  });

  it("step duration equals max of effect durations", () => {
    // SpellCast (500) + ZoneChanged (400) => step duration = 500
    const events: GameEvent[] = [
      { type: "SpellCast", data: { card_id: 1, controller: 0, object_id: 1 } },
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
      { type: "SpellCast", data: { card_id: 1, controller: 0, object_id: 1 } },
      { type: "ZoneChanged", data: { object_id: 1, from: "Hand", to: "Stack" } },
      { type: "PriorityPassed", data: { player_id: 1 } },
      // Attacker 1 hits blockers 2 and 3 — each is a separate sequential step
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 3 } },
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 3 }, amount: 2 } },
      { type: "LifeChanged", data: { player_id: 1, amount: -5 } },
      { type: "CreatureDestroyed", data: { object_id: 2 } },
      { type: "CreatureDestroyed", data: { object_id: 3 } },
    ];

    const steps = normalizeEvents(events);
    // Step 1: SpellCast + ZoneChanged
    // Step 2: DamageDealt 1→2
    // Step 3: DamageDealt 1→3 + LifeChanged (merges into last step)
    // Step 4: CreatureDestroyed x2
    expect(steps).toHaveLength(4);
    expect(steps[0].effects).toHaveLength(2);
    expect(steps[1].effects).toHaveLength(1);
    expect(steps[2].effects).toHaveLength(2);
    expect(steps[3].effects).toHaveLength(2);
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
