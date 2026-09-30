import { describe, expect, it } from "vitest";

import type { GameObject, GameState, ManaColor, StackEntry, TargetRef } from "../../adapter/types.ts";
import { buildObjectMap, gameObjectFactory } from "../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildStackEntry } from "../../test/factories/gameStateFactory.ts";
import { damageCauseOf } from "../damageCause.ts";
import {
  type AnimationEvent,
  CARD_SLAM_FLIGHT_MS,
  DAMAGE_CAUSE_IMPACT_MS,
  impactDelayMsForAnimationEvent,
} from "../types.ts";

const SPELL = 20;
const PINGER = 21;
const ABILITY = 30;

const shock = gameObjectFactory.withId(SPELL).named("Shock").instant().params({ zone: "Stack", color: ["Red"] });
const pinger = gameObjectFactory.withId(PINGER).named("Prodigal Sorcerer").creature(1, 1).onBattlefield();

function state(objects: GameObject[], stack: StackEntry[]): GameState {
  return buildGameState({ objects: buildObjectMap(...objects), stack });
}

function damage(sourceId: number, target: TargetRef = { Player: 1 }, isCombat = false): AnimationEvent {
  return { type: "DamageDealt", data: { source_id: sourceId, target, amount: 2, is_combat: isCombat } };
}

const castShock = buildStackEntry({ id: SPELL, source_id: SPELL });
const pingerAbility = buildStackEntry({
  id: ABILITY,
  source_id: PINGER,
  kind: { type: "ActivatedAbility", data: { source_id: PINGER, ability: { targets: [] } } },
});

describe("damageCauseOf", () => {
  it("V10-5: a resolving spell's damage leaves from its stack entry", () => {
    expect(damageCauseOf(damage(SPELL), state([shock.build()], [castShock]))).toEqual({
      cause: "fire",
      origin: { zone: "Stack", objectId: SPELL, ownerId: 0 },
    });
  });

  it("V10-5: a resolving ability's damage leaves from its source permanent, or its entry once the source has gone", () => {
    const pre = state([pinger.params({ color: ["Blue"] }).build()], [pingerAbility]);
    expect(damageCauseOf(damage(PINGER), pre)).toEqual({
      cause: "water",
      origin: { zone: "Battlefield", objectId: PINGER, ownerId: 0 },
    });

    const gone = state([pinger.params({ color: ["Red"] }).inGraveyard().build()], [pingerAbility]);
    expect(damageCauseOf(damage(PINGER), gone)?.origin).toEqual({ zone: "Stack", objectId: ABILITY, ownerId: 0 });
  });

  it("V10-5: a resolution resuming after a choice is the engine's resolving entry, not the stack's top", () => {
    const other = buildStackEntry({ id: 40, source_id: 41 });
    const resumed = { ...state([pinger.build()], [other]), resolving_stack_entry: pingerAbility };
    expect(damageCauseOf(damage(PINGER), resumed)?.origin).toEqual({ zone: "Battlefield", objectId: PINGER, ownerId: 0 });
  });

  it.each<[readonly ManaColor[], string]>([
    [["Red"], "fire"],
    [["Black", "Red"], "fire"],
    [["Blue"], "water"],
    [["White", "Blue"], "water"],
    [["Blue", "Red"], "lightning"],
    [[], "lightning"],
  ])("V10-5: a %j source's damage is %s", (color, cause) => {
    const pre = state([shock.params({ color: [...color] }).build()], [castShock]);
    expect(damageCauseOf(damage(SPELL), pre)?.cause).toBe(cause);
  });

  it("V10-5: combat damage, damage from anything but the resolving spell or ability, and no state have no cause", () => {
    const pre = state([shock.build(), pinger.build()], [castShock]);
    expect(damageCauseOf(damage(SPELL, { Player: 1 }, true), pre)).toBeNull();
    // A spell that makes a creature deal damage: the creature is not what resolves.
    expect(damageCauseOf(damage(PINGER), pre)).toBeNull();
    expect(damageCauseOf(damage(SPELL), state([shock.build()], []))).toBeNull();
    expect(damageCauseOf(damage(SPELL), null)).toBeNull();
  });
});

describe("impactDelayMsForAnimationEvent", () => {
  it("V10-6: player damage lands when a damage cause arrives if card VFX present it, else with the card slam", () => {
    const pre = state([shock.build(), pinger.build()], [castShock]);
    expect(impactDelayMsForAnimationEvent(damage(SPELL), pre)).toBe(DAMAGE_CAUSE_IMPACT_MS);
    expect(impactDelayMsForAnimationEvent(damage(SPELL), null)).toBe(CARD_SLAM_FLIGHT_MS);
    expect(impactDelayMsForAnimationEvent(damage(PINGER), pre)).toBe(CARD_SLAM_FLIGHT_MS);
    expect(impactDelayMsForAnimationEvent(damage(SPELL, { Object: PINGER }), pre)).toBe(0);
  });
});
