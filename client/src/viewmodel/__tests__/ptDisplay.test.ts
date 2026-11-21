import { describe, expect, it } from "vitest";

import type { GameObject } from "../../adapter/types";
import { computePTDisplay } from "../cardProps";

function makeGameObject(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 1,
    card_id: 100,
    owner: 0,
    controller: 0,
    zone: "Battlefield",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name: "Test Creature",
    power: 3,
    toughness: 4,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "NoCost" },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],

    color: ["Green"],
    base_power: 3,
    base_toughness: 4,
    base_keywords: [],
    base_color: ["Green"],
    timestamp: 1,
    entered_battlefield_turn: null,
    ...overrides,
  };
}

describe("computePTDisplay", () => {
  it("returns null for non-creatures", () => {
    const obj = makeGameObject({
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
    });
    expect(computePTDisplay(obj)).toBeNull();
  });

  it("returns white colors for base stats", () => {
    const obj = makeGameObject({ power: 3, toughness: 4, base_power: 3, base_toughness: 4 });
    const display = computePTDisplay(obj)!;

    expect(display.power).toBe("3");
    expect(display.toughness).toBe("4");
    expect(display.powerColor).toBe("white");
    expect(display.toughnessColor).toBe("white");
  });

  it("returns green power color when buffed", () => {
    const obj = makeGameObject({ power: 5, base_power: 3 });
    const display = computePTDisplay(obj)!;

    expect(display.powerColor).toBe("green");
  });

  it("returns red power color when debuffed", () => {
    const obj = makeGameObject({ power: 1, base_power: 3 });
    const display = computePTDisplay(obj)!;

    expect(display.powerColor).toBe("red");
  });

  it("returns red toughness color and reduced value when damaged", () => {
    const obj = makeGameObject({ damage_marked: 2 });
    const display = computePTDisplay(obj)!;

    expect(display.toughness).toBe("2");
    expect(display.toughnessColor).toBe("red");
  });

  it("returns green toughness color when buffed", () => {
    const obj = makeGameObject({ toughness: 6, base_toughness: 4 });
    const display = computePTDisplay(obj)!;

    expect(display.toughnessColor).toBe("green");
  });

  it("returns red toughness color when debuffed", () => {
    const obj = makeGameObject({ toughness: 2, base_toughness: 4 });
    const display = computePTDisplay(obj)!;

    expect(display.toughnessColor).toBe("red");
  });

  it("prioritizes damage over buff for toughness color", () => {
    const obj = makeGameObject({ toughness: 6, base_toughness: 4, damage_marked: 1 });
    const display = computePTDisplay(obj)!;

    // damage_marked > 0 takes priority
    expect(display.toughness).toBe("5");
    expect(display.toughnessColor).toBe("red");
  });

  // Unhinged half points (Vile Bile 2½/2½, Little Girl ½/½): the engine sends a
  // `_fraction` beside each whole field, and the label shows the printed value.
  it("shows exact half-point power and toughness", () => {
    const obj = makeGameObject({
      name: "Vile Bile",
      power: 2,
      power_fraction: "Half",
      toughness: 2,
      toughness_fraction: "Half",
      base_power: 2,
      base_power_fraction: "Half",
      base_toughness: 2,
      base_toughness_fraction: "Half",
    });
    const display = computePTDisplay(obj)!;

    expect(display.power).toBe("2½");
    expect(display.toughness).toBe("2½");
    expect(display.base).toBe("2½/2½");
    expect(display.powerColor).toBe("white");
    expect(display.toughnessColor).toBe("white");
  });

  it("shows a half-only value and subtracts exact half-point damage", () => {
    const girl = computePTDisplay(makeGameObject({
      power: 0,
      power_fraction: "Half",
      toughness: 0,
      toughness_fraction: "Half",
      base_power: 0,
      base_power_fraction: "Half",
      base_toughness: 0,
      base_toughness_fraction: "Half",
    }))!;
    expect(`${girl.power}/${girl.toughness}`).toBe("½/½");

    // A 3/3 with Vile Bile's 2½ marked has ½ toughness left.
    const giant = computePTDisplay(makeGameObject({
      power: 3,
      toughness: 3,
      base_power: 3,
      base_toughness: 3,
      damage_marked: 2,
      damage_marked_fraction: "Half",
    }))!;
    expect(giant.toughness).toBe("½");
    expect(giant.toughnessColor).toBe("red");
  });

  it("compares half-point buffs exactly", () => {
    // 3 power over a 2½ base is a buff; a whole-only comparison (3 vs 2) agrees,
    // but 2½ over a 2½ base is not, and 2 under it is a debuff.
    const pumped = computePTDisplay(makeGameObject({
      power: 3, base_power: 2, base_power_fraction: "Half",
    }))!;
    expect(pumped.powerColor).toBe("green");
    const shrunk = computePTDisplay(makeGameObject({
      power: 2, base_power: 2, base_power_fraction: "Half",
    }))!;
    expect(shrunk.powerColor).toBe("red");
  });
});
