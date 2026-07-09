import { describe, expect, it } from "vitest";

import type { GameObject } from "../../adapter/types";
import { toCardProps, toRoman } from "../cardProps";

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

describe("toCardProps", () => {
  it("maps basic fields from GameObject", () => {
    const obj = makeGameObject({ id: 42, name: "Grizzly Bears", tapped: true });
    const props = toCardProps(obj);

    expect(props.id).toBe(42);
    expect(props.name).toBe("Grizzly Bears");
    expect(props.tapped).toBe(true);
    expect(props.power).toBe(3);
    expect(props.toughness).toBe(4);
    expect(props.basePower).toBe(3);
    expect(props.baseToughness).toBe(4);
    expect(props.damageMarked).toBe(0);
  });

  it("detects power buffed when power > base_power", () => {
    const obj = makeGameObject({ power: 5, base_power: 3 });
    const props = toCardProps(obj);

    expect(props.isPowerBuffed).toBe(true);
    expect(props.isPowerDebuffed).toBe(false);
  });

  it("detects power debuffed when power < base_power", () => {
    const obj = makeGameObject({ power: 1, base_power: 3 });
    const props = toCardProps(obj);

    expect(props.isPowerBuffed).toBe(false);
    expect(props.isPowerDebuffed).toBe(true);
  });

  it("detects toughness buffed when toughness > base_toughness", () => {
    const obj = makeGameObject({ toughness: 6, base_toughness: 4 });
    const props = toCardProps(obj);

    expect(props.isToughnessBuffed).toBe(true);
    expect(props.isToughnessDebuffed).toBe(false);
  });

  it("detects toughness debuffed when toughness < base_toughness", () => {
    const obj = makeGameObject({ toughness: 2, base_toughness: 4 });
    const props = toCardProps(obj);

    expect(props.isToughnessDebuffed).toBe(true);
  });

  it("detects toughness debuffed when damage_marked > 0", () => {
    const obj = makeGameObject({ damage_marked: 2 });
    const props = toCardProps(obj);

    expect(props.isToughnessDebuffed).toBe(true);
  });

  it("computes effectiveToughness as toughness minus damage", () => {
    const obj = makeGameObject({ toughness: 4, damage_marked: 1 });
    const props = toCardProps(obj);

    expect(props.effectiveToughness).toBe(3);
  });

  it("returns null effectiveToughness for non-creatures", () => {
    const obj = makeGameObject({
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: [] },
    });
    const props = toCardProps(obj);

    expect(props.effectiveToughness).toBeNull();
  });

  it("uses public display name for face-down cards", () => {
    const obj = makeGameObject({
      face_down: true,
      name: "Hidden Sorcery",
    });

    const props = toCardProps(obj);

    expect(props.name).toBe("Face-down card");
    expect(props.power).toBe(3);
    expect(props.toughness).toBe(4);
  });

  it("extracts counters as typed array", () => {
    const obj = makeGameObject({ counters: { P1P1: 2, loyalty: 3 } });
    const props = toCardProps(obj);

    expect(props.counters).toEqual([
      { type: "P1P1", count: 2 },
      { type: "loyalty", count: 3 },
    ]);
  });

  it("detects creature and land types", () => {
    const creature = makeGameObject({
      card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Elf"] },
    });
    expect(toCardProps(creature).isCreature).toBe(true);
    expect(toCardProps(creature).isLand).toBe(false);

    const land = makeGameObject({
      card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] },
    });
    expect(toCardProps(land).isCreature).toBe(false);
    expect(toCardProps(land).isLand).toBe(true);
  });

  it("maps attachments and keywords", () => {
    const obj = makeGameObject({
      attached_to: { type: "Object", data: 5 },
      attachments: [10, 11],
      keywords: ["Flying", "Trample"],
      color: ["White", "Blue"],
    });
    const props = toCardProps(obj);

    expect(props.attachedTo).toEqual({ type: "Object", data: 5 });
    expect(props.attachmentIds).toEqual([10, 11]);
    expect(props.keywords).toEqual(["Flying", "Trample"]);
    expect(props.colorIdentity).toEqual(["White", "Blue"]);
  });
});

describe("toRoman", () => {
  it("converts 1-5 to Roman numerals", () => {
    expect(["", "I", "II", "III", "IV", "V"].map((_, i) => toRoman(i))).toEqual([
      "", "I", "II", "III", "IV", "V",
    ]);
  });

  it("renders blank (never the literal 'undefined'/'NaN') for a missing/invalid level", () => {
    expect(toRoman(undefined as unknown as number)).toBe("");
    expect(toRoman(NaN)).toBe("");
    expect(toRoman(0)).toBe("");
    expect(toRoman(-1)).toBe("");
  });

  it("falls back to the arabic numeral for a Class level beyond V", () => {
    expect(toRoman(6)).toBe("6");
  });
});
