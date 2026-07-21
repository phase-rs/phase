import { describe, expect, it } from "vitest";

import { gameObjectFactory } from "../gameObjectFactory.ts";
import { gameStateFactory, waitingForFactory } from "../gameStateFactory.ts";

describe("gameObjectFactory convenience methods", () => {
  it("composes card type, supertype, zone, and state methods", () => {
    const object = gameObjectFactory
      .creature(3, 4)
      .legendary()
      .inHand()
      .named("Sigarda")
      .withId(7)
      .build();

    expect(object.card_types.core_types).toEqual(["Creature"]);
    expect(object.card_types.supertypes).toEqual(["Legendary"]);
    expect(object.power).toBe(3);
    expect(object.toughness).toBe(4);
    expect(object.base_power).toBe(3);
    expect(object.base_toughness).toBe(4);
    expect(object.zone).toBe("Hand");
    expect(object.entered_battlefield_turn).toBeNull();
    expect(object.name).toBe("Sigarda");
    expect(object.id).toBe(7);
    expect(object.card_id).toBe(7);
  });

  it("auto-increments ids so unlabeled builds never collide", () => {
    const first = gameObjectFactory.build();
    const second = gameObjectFactory.build();

    expect(first.id).not.toBe(second.id);
    expect(first.card_id).toBe(first.id);
  });

  it("builds a commander in the command zone", () => {
    const commander = gameObjectFactory.commander().build();

    expect(commander.zone).toBe("Command");
    expect(commander.is_commander).toBe(true);
    expect(commander.commander_tax).toBe(0);
    expect(commander.card_types.supertypes).toEqual(["Legendary"]);
    expect(commander.card_types.core_types).toEqual(["Creature"]);
  });
});

describe("waitingForFactory", () => {
  it("defaults to Priority", () => {
    expect(waitingForFactory.build()).toEqual({
      type: "Priority",
      data: { player: 0 },
    });
  });

  it("switches variants without leaking keys from the default variant", () => {
    const waitingFor = waitingForFactory.assistPayment().build();

    // AssistPayment data must not inherit `player` from the Priority default.
    expect(waitingFor).toEqual({
      type: "AssistPayment",
      data: { caster: 1, chosen: 0, max_generic: 0 },
    });
  });

  it("applies data overrides onto variant defaults", () => {
    const waitingFor = waitingForFactory.targetSelection({ player: 1 }).build();

    expect(waitingFor).toMatchObject({
      type: "TargetSelection",
      data: expect.objectContaining({ player: 1 }),
    });
  });
});

describe("gameStateFactory convenience methods", () => {
  it("replaces waiting_for exactly via variant methods", () => {
    const state = gameStateFactory.manaPayment(1).build();

    expect(state.waiting_for).toEqual({
      type: "ManaPayment",
      data: { player: 1 },
    });
  });

  it("derives objects map, battlefield, and next_object_id from withObjects", () => {
    const bear = gameObjectFactory.creature().onBattlefield().withId(3).build();
    const bolt = gameObjectFactory.instant().inHand().withId(9).build();

    const state = gameStateFactory.withObjects(bear, bolt).build();

    expect(state.objects).toEqual({ "3": bear, "9": bolt });
    expect(state.battlefield).toEqual([3]);
    expect(state.next_object_id).toBe(10);
  });

  it("derives seat_order from withPlayers", () => {
    const state = gameStateFactory.withPlayers(0, 1, { id: 2, life: 12 }).build();

    expect(state.players).toHaveLength(3);
    expect(state.players[2].life).toBe(12);
    expect(state.seat_order).toEqual([0, 1, 2]);
  });
});
