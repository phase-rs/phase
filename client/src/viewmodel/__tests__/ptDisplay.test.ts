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

    expect(display.power).toBe(3);
    expect(display.toughness).toBe(4);
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

    expect(display.toughness).toBe(2);
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
    expect(display.toughness).toBe(5);
    expect(display.toughnessColor).toBe("red");
  });
});
