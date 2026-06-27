import { describe, expect, it } from "vitest";

import type { GameEvent } from "../../adapter/types";
import type { AnimationStep } from "../types";
import { normalizeEvents } from "../eventNormalizer";
import {
  EVENT_DURATIONS,
  GROUPED_COMBAT_DAMAGE_DURATION_MS,
  GROUPED_COMBAT_DAMAGE_THRESHOLD,
  GROUPED_DAMAGE_FLURRY_IMPACT_DELAY_MS,
} from "../types";

function combatPlayerDamage(sourceId: number, playerId = 0, amount = 1): GameEvent {
  return { type: "DamageDealt", data: { source_id: sourceId, target: { Player: playerId }, amount, is_combat: true } };
}

function lifeChanged(playerId = 0, amount = -1): GameEvent {
  return { type: "LifeChanged", data: { player_id: playerId, amount } };
}

function poisonCounterChanged(playerId = 0, amount = 1): GameEvent {
  return { type: "PlayerCounterChanged", data: { player: playerId, counter_kind: "Poison", delta: amount } };
}

function combatAggregate(sourceIds: number[], playerId = 0, amount = 1): GameEvent {
  return {
    type: "CombatDamageDealtToPlayer",
    data: {
      player_id: playerId,
      source_amounts: sourceIds.map((sourceId) => [sourceId, amount]),
      total_damage: sourceIds.length * amount,
    },
  };
}

function expectGroupedFlurry(event: AnimationStep["effects"][number]["event"]) {
  expect(event.type).toBe("GroupedDamageFlurry");
  if (event.type !== "GroupedDamageFlurry") throw new Error("Expected GroupedDamageFlurry");
  return event;
}

function expectLifeChanged(event: AnimationStep["effects"][number]["event"]) {
  expect(event.type).toBe("LifeChanged");
  if (event.type !== "LifeChanged") throw new Error("Expected LifeChanged");
  return event;
}

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
      { type: "CardDrawn", data: { player_id: 0, object_id: 1, nth_in_turn: 1, nth_in_step: 1 } },
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
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 3, is_combat: false } },
      { type: "DamageDealt", data: { source_id: 2, target: { Object: 1 }, amount: 2, is_combat: false } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(1);
    expect(steps[0].effects).toHaveLength(2);
  });

  it("DamageDealt: each attacker's engagement is a separate step", () => {
    // Attacker 1 hits blocker 2; unrelated attacker 4 hits player
    const events: GameEvent[] = [
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 3, is_combat: false } },
      { type: "DamageDealt", data: { source_id: 4, target: { Player: 0 }, amount: 5, is_combat: false } },
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
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 2, is_combat: false } },
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 3 }, amount: 1, is_combat: false } },
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
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 2, is_combat: false } },
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 3 }, amount: 1, is_combat: false } },
      { type: "DamageDealt", data: { source_id: 2, target: { Object: 1 }, amount: 2, is_combat: false } },
      { type: "DamageDealt", data: { source_id: 3, target: { Object: 1 }, amount: 1, is_combat: false } },
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
      { type: "DamageDealt", data: { source_id: 1, target: { Player: 0 }, amount: 3, is_combat: false } },
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

  it("BlockersDeclared is non-visual (no animation step)", () => {
    const events: GameEvent[] = [
      { type: "BlockersDeclared", data: { assignments: [[3, 1]] } },
    ];

    const steps = normalizeEvents(events);
    expect(steps).toHaveLength(0);
  });

  it("combat pacing scales combat-only step durations", () => {
    const events: GameEvent[] = [
      { type: "DamageDealt", data: { source_id: 1, target: { Player: 0 }, amount: 3, is_combat: false } },
      { type: "SpellCast", data: { card_id: 9, controller: 0, object_id: 9 } },
    ];

    const steps = normalizeEvents(events, {
      pacingMultipliers: { effects: 1.0, combat: 1.75, banners: 1.0 },
    });
    expect(steps).toHaveLength(2);
    expect(steps[0].duration).toBeGreaterThan(EVENT_DURATIONS.DamageDealt);
    expect(steps[1].duration).toBe(EVENT_DURATIONS.SpellCast);
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
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 3, is_combat: false } },
      { type: "DamageDealt", data: { source_id: 1, target: { Object: 3 }, amount: 2, is_combat: false } },
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

  it("groups large aggregate combat damage with following LifeChanged pairs into one flurry step", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const events = [
      ...sources.flatMap((sourceId) => [combatPlayerDamage(sourceId), lifeChanged()]),
      combatAggregate(sources),
    ];

    const steps = normalizeEvents(events);

    expect(steps).toHaveLength(1);
    expect(expectGroupedFlurry(steps[0].effects[0].event).data.hit_count).toBe(sources.length);
    expect(steps[0].effects[1]).toMatchObject({
      event: { type: "LifeChanged", data: { player_id: 0, amount: -sources.length } },
      displayOnly: true,
    });
  });

  it("groups aggregate combat damage when replacement effects change life-loss amount", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const events = [
      ...sources.flatMap((sourceId) => [combatPlayerDamage(sourceId), lifeChanged(0, -2)]),
      combatAggregate(sources),
    ];

    const steps = normalizeEvents(events);

    expect(steps).toHaveLength(1);
    expect(expectLifeChanged(steps[0].effects[1].event).data.amount).toBe(-sources.length * 2);
  });

  it("groups large aggregate combat damage with preceding LifeChanged pairs", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const events = [
      ...sources.flatMap((sourceId) => [lifeChanged(), combatPlayerDamage(sourceId)]),
      combatAggregate(sources),
    ];

    const steps = normalizeEvents(events);

    expect(steps).toHaveLength(1);
    expect(steps[0].effects[0].event.type).toBe("GroupedDamageFlurry");
    expect(expectLifeChanged(steps[0].effects[1].event).data.amount).toBe(-sources.length);
  });

  it("groups large toxic combat damage through poison side effects", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const steps = normalizeEvents([
      ...sources.flatMap((sourceId) => [
        lifeChanged(),
        poisonCounterChanged(),
        combatPlayerDamage(sourceId),
      ]),
      combatAggregate(sources),
    ]);

    expect(steps).toHaveLength(1);
    expect(steps[0].effects.map((effect) => effect.event.type)).toEqual([
      "GroupedDamageFlurry",
      "LifeChanged",
    ]);
    expect(expectLifeChanged(steps[0].effects[1].event).data.amount).toBe(-sources.length);
  });

  it("groups large infect combat damage without synthesizing life loss", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const steps = normalizeEvents([
      ...sources.flatMap((sourceId) => [
        poisonCounterChanged(),
        combatPlayerDamage(sourceId),
      ]),
      combatAggregate(sources),
    ]);

    expect(steps).toHaveLength(1);
    expect(steps[0].effects.map((effect) => effect.event.type)).toEqual(["GroupedDamageFlurry"]);
  });

  it("keeps same-player aggregate matching bounded to the current combat aggregate segment", () => {
    const firstSources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const secondSources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 101);
    const events = [
      ...firstSources.flatMap((sourceId) => [combatPlayerDamage(sourceId), lifeChanged()]),
      combatAggregate(firstSources),
      ...secondSources.flatMap((sourceId) => [combatPlayerDamage(sourceId), lifeChanged()]),
      combatAggregate(secondSources),
    ];

    const steps = normalizeEvents(events);

    expect(steps).toHaveLength(2);
    expect(steps.map((step) => step.effects[0].event.type)).toEqual(["GroupedDamageFlurry", "GroupedDamageFlurry"]);
    expect(expectGroupedFlurry(steps[1].effects[0].event).data.source_ids).toEqual(secondSources);
  });

  it("groups aggregate combat damage without synthesizing life when no LifeChanged was consumed", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const steps = normalizeEvents([
      ...sources.map((sourceId) => combatPlayerDamage(sourceId)),
      combatAggregate(sources),
    ]);

    expect(steps).toHaveLength(1);
    expect(steps[0].effects.map((effect) => effect.event.type)).toEqual(["GroupedDamageFlurry"]);
  });

  it("ignores zero-damage aggregate source amounts when grouping", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const events: GameEvent[] = [
      ...sources.map((sourceId) => combatPlayerDamage(sourceId)),
      {
        type: "CombatDamageDealtToPlayer",
        data: {
          player_id: 0,
          source_amounts: [[999, 0], ...sources.map((sourceId) => [sourceId, 1] as [number, number])],
          total_damage: sources.length,
        },
      },
    ];

    const steps = normalizeEvents(events);

    expect(steps).toHaveLength(1);
    expect(expectGroupedFlurry(steps[0].effects[0].event).data.hit_count).toBe(sources.length);
    expect(expectGroupedFlurry(steps[0].effects[0].event).data.source_ids).toEqual(sources);
  });

  it("carries lifelink life gain into the grouped combat impact step", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const steps = normalizeEvents([
      ...sources.map((sourceId) => combatPlayerDamage(sourceId)),
      lifeChanged(1, sources.length),
      combatAggregate(sources),
    ]);

    expect(steps).toHaveLength(1);
    expect(steps[0].effects.map((effect) => effect.event.type)).toEqual([
      "GroupedDamageFlurry",
      "LifeChanged",
    ]);
    expect(steps[0].effects[1]).toMatchObject({
      event: { type: "LifeChanged", data: { player_id: 1, amount: sources.length } },
      displayOnly: true,
    });
  });

  it("does not fold unrelated earlier life gain into the grouped combat step", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const steps = normalizeEvents([
      lifeChanged(0, 3),
      ...sources.map((sourceId) => combatPlayerDamage(sourceId)),
      combatAggregate(sources),
    ]);

    expect(steps).toHaveLength(2);
    expect(steps[0].effects[0]).toMatchObject({
      event: { type: "LifeChanged", data: { player_id: 0, amount: 3 } },
    });
    expect(steps[1].effects.map((effect) => effect.event.type)).toEqual(["GroupedDamageFlurry"]);
  });

  it("defers unattributed lifelink gain until after consecutive grouped player aggregates", () => {
    const firstSources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const secondSources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 101);
    const steps = normalizeEvents([
      ...firstSources.map((sourceId) => combatPlayerDamage(sourceId, 0)),
      ...secondSources.map((sourceId) => combatPlayerDamage(sourceId, 2)),
      lifeChanged(1, firstSources.length + secondSources.length),
      combatAggregate(firstSources, 0),
      combatAggregate(secondSources, 2),
    ]);

    expect(steps).toHaveLength(3);
    expect(steps[0].effects[0].event.type).toBe("GroupedDamageFlurry");
    expect(expectGroupedFlurry(steps[0].effects[0].event).data.player_id).toBe(0);
    expect(steps[1].effects[0].event.type).toBe("GroupedDamageFlurry");
    expect(expectGroupedFlurry(steps[1].effects[0].event).data.player_id).toBe(2);
    expect(steps[2].effects[0]).toMatchObject({
      event: { type: "LifeChanged", data: { player_id: 1, amount: firstSources.length + secondSources.length } },
      displayOnly: true,
    });
  });

  it("does not consume non-loss life changes or synthesize life from aggregate total damage", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const steps = normalizeEvents([
      ...sources.map(() => lifeChanged(0, 2)),
      ...sources.map((sourceId) => combatPlayerDamage(sourceId)),
      combatAggregate(sources),
    ]);

    expect(steps).toHaveLength(2);
    expect(steps[0].effects).toHaveLength(GROUPED_COMBAT_DAMAGE_THRESHOLD);
    expect(steps[0].effects.every((effect) => effect.event.type === "LifeChanged")).toBe(true);
    const grouped = steps[steps.length - 1];
    expect(grouped.effects.map((effect) => effect.event.type)).toEqual(["GroupedDamageFlurry"]);
  });

  it("aborts aggregate grouping when the aggregate cannot match every source amount", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const events = [
      ...sources.slice(1).map((sourceId) => combatPlayerDamage(sourceId)),
      combatAggregate(sources),
    ];

    const steps = normalizeEvents(events);

    expect(steps.some((step) => step.effects[0].event.type === "GroupedDamageFlurry")).toBe(false);
    expect(steps).toHaveLength(GROUPED_COMBAT_DAMAGE_THRESHOLD - 1);
  });

  it("does not fall back to adjacent grouping inside a failed aggregate segment", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD + 1 }, (_, i) => i + 1);
    const events = [
      ...sources.slice(1).map((sourceId) => combatPlayerDamage(sourceId)),
      combatAggregate(sources),
    ];

    const steps = normalizeEvents(events);

    expect(steps.some((step) => step.effects[0].event.type === "GroupedDamageFlurry")).toBe(false);
    expect(steps).toHaveLength(GROUPED_COMBAT_DAMAGE_THRESHOLD);
  });

  it("falls back to adjacent-run grouping when aggregate source amounts are missing", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const events: GameEvent[] = [
      ...sources.flatMap((sourceId) => [combatPlayerDamage(sourceId), lifeChanged()]),
      { type: "CombatDamageDealtToPlayer", data: { player_id: 0, total_damage: sources.length } },
    ];

    const steps = normalizeEvents(events);

    expect(steps).toHaveLength(1);
    expect(steps[0].effects[0].event.type).toBe("GroupedDamageFlurry");
  });

  it("falls back when replacement effects change life-loss amount", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const events: GameEvent[] = [
      ...sources.flatMap((sourceId) => [combatPlayerDamage(sourceId), lifeChanged(0, -2)]),
      { type: "CombatDamageDealtToPlayer", data: { player_id: 0, total_damage: sources.length } },
    ];

    const steps = normalizeEvents(events);

    expect(steps).toHaveLength(1);
    expect(expectLifeChanged(steps[0].effects[1].event).data.amount).toBe(-sources.length * 2);
  });

  it("falls back with post-damage lifelink gain when aggregate source amounts are missing", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const events: GameEvent[] = [
      ...sources.map((sourceId) => combatPlayerDamage(sourceId)),
      lifeChanged(1, sources.length),
      { type: "CombatDamageDealtToPlayer", data: { player_id: 0, total_damage: sources.length } },
    ];

    const steps = normalizeEvents(events);

    expect(steps).toHaveLength(1);
    expect(steps[0].effects.map((effect) => effect.event.type)).toEqual([
      "GroupedDamageFlurry",
      "LifeChanged",
    ]);
    expect(steps[0].effects[1]).toMatchObject({
      event: { type: "LifeChanged", data: { player_id: 1, amount: sources.length } },
      displayOnly: true,
    });
  });

  it("falls back through toxic poison side effects when aggregate source amounts are missing", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const events: GameEvent[] = [
      ...sources.flatMap((sourceId) => [
        lifeChanged(),
        poisonCounterChanged(),
        combatPlayerDamage(sourceId),
      ]),
      { type: "CombatDamageDealtToPlayer", data: { player_id: 0, total_damage: sources.length } },
    ];

    const steps = normalizeEvents(events);

    expect(steps).toHaveLength(1);
    expect(steps[0].effects.map((effect) => effect.event.type)).toEqual([
      "GroupedDamageFlurry",
      "LifeChanged",
    ]);
  });

  it("falls back through infect poison side effects without synthesizing life", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const events: GameEvent[] = [
      ...sources.flatMap((sourceId) => [
        poisonCounterChanged(),
        combatPlayerDamage(sourceId),
      ]),
      { type: "CombatDamageDealtToPlayer", data: { player_id: 0, total_damage: sources.length } },
    ];

    const steps = normalizeEvents(events);

    expect(steps).toHaveLength(1);
    expect(steps[0].effects.map((effect) => effect.event.type)).toEqual(["GroupedDamageFlurry"]);
  });

  it("groups large adjacent combat player damage pairs without an aggregate", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const steps = normalizeEvents(sources.flatMap((sourceId) => [combatPlayerDamage(sourceId), lifeChanged()]));

    expect(steps).toHaveLength(1);
    expect(steps[0].effects[0].event.type).toBe("GroupedDamageFlurry");
    expect(expectLifeChanged(steps[0].effects[1].event).data.amount).toBe(-sources.length);
  });

  it("keeps small combat player batches per attacker", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD - 1 }, (_, i) => i + 1);
    const steps = normalizeEvents(sources.map((sourceId) => combatPlayerDamage(sourceId)));

    expect(steps).toHaveLength(sources.length);
    expect(steps.every((step) => step.effects[0].event.type === "DamageDealt")).toBe(true);
  });

  it("does not group non-combat or object-target combat damage", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const nonCombat = normalizeEvents(
      sources.map((sourceId) => ({
        type: "DamageDealt",
        data: { source_id: sourceId, target: { Player: 0 }, amount: 1, is_combat: false },
      })),
    );
    const objectTarget = normalizeEvents(
      sources.map((sourceId) => ({
        type: "DamageDealt",
        data: { source_id: sourceId, target: { Object: 99 }, amount: 1, is_combat: true },
      })),
    );

    expect(nonCombat.some((step) => step.effects[0].event.type === "GroupedDamageFlurry")).toBe(false);
    expect(objectTarget.some((step) => step.effects[0].event.type === "GroupedDamageFlurry")).toBe(false);
  });

  it("uses capped combat-paced duration for grouped flurries", () => {
    const sources = Array.from({ length: 700 }, (_, i) => i + 1);
    const steps = normalizeEvents(sources.map((sourceId) => combatPlayerDamage(sourceId)), {
      pacingMultipliers: { effects: 1, combat: 1.5, banners: 1 },
    });

    expect(steps).toHaveLength(1);
    expect(steps[0].duration).toBe(GROUPED_COMBAT_DAMAGE_DURATION_MS * 1.5);
  });

  it("keeps grouped flurry duration at least as long as its impact delay", () => {
    const sources = Array.from({ length: GROUPED_COMBAT_DAMAGE_THRESHOLD }, (_, i) => i + 1);
    const steps = normalizeEvents(sources.map((sourceId) => combatPlayerDamage(sourceId)), {
      pacingMultipliers: { effects: 1, combat: 0, banners: 1 },
    });

    expect(steps).toHaveLength(1);
    expect(steps[0].duration).toBe(GROUPED_DAMAGE_FLURRY_IMPACT_DELAY_MS);
  });
});
