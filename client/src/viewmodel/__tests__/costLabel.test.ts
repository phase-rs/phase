import { describe, expect, it } from "vitest";

import type { GameAction, GameObject, Keyword } from "../../adapter/types.ts";
import { abilityChoiceLabel } from "../costLabel.ts";

function makeObject(overrides: Partial<GameObject> = {}): GameObject {
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
    name: "Test Card",
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: [], subtypes: [] },
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
    back_face: null,
    ...overrides,
  };
}

describe("abilityChoiceLabel per-variant formatting", () => {
  it("labels CrewVehicle with the keyword N extracted from engine keywords", () => {
    const object = makeObject({
      name: "Skysovereign, Consul Flagship",
      keywords: [{ Crew: 3 } as Keyword],
    });
    const action: GameAction = {
      type: "CrewVehicle",
      data: { vehicle_id: 1, creature_ids: [] },
    };
    const result = abilityChoiceLabel(action, object);
    expect(result.label).toBe("Crew 3");
    expect(result.description).toContain("total power 3 or greater");
  });

  it("falls back to 'Crew' when no Crew keyword is present (defensive)", () => {
    // Should never happen in practice, but guards against malformed data.
    const object = makeObject({ name: "Phantom Vehicle", keywords: [] });
    const action: GameAction = {
      type: "CrewVehicle",
      data: { vehicle_id: 1, creature_ids: [] },
    };
    expect(abilityChoiceLabel(action, object).label).toBe("Crew");
  });

  it("labels SaddleMount with Saddle N extracted from keywords", () => {
    const object = makeObject({
      name: "Rodeo Pyrohelix",
      keywords: [{ Saddle: 2 } as Keyword],
    });
    const action: GameAction = {
      type: "SaddleMount",
      data: { mount_id: 1, creature_ids: [] },
    };
    const result = abilityChoiceLabel(action, object);
    expect(result.label).toBe("Saddle 2");
    expect(result.description).toContain("total power 2 or greater");
  });

  it("labels ActivateStation with a fixed label and rules-text description", () => {
    const object = makeObject({
      name: "Monoist Gravliner",
      keywords: ["Station" as Keyword],
    });
    const action: GameAction = {
      type: "ActivateStation",
      data: { spacecraft_id: 1, creature_id: null },
    };
    const result = abilityChoiceLabel(action, object);
    expect(result.label).toBe("Station");
    expect(result.description).toContain("charge counters equal to its power");
  });

  it("labels Equip with a fixed label and rules-text description", () => {
    const object = makeObject({ name: "Sword of Feast and Famine" });
    const action: GameAction = {
      type: "Equip",
      data: { equipment_id: 1, target_id: 5 },
    };
    const result = abilityChoiceLabel(action, object);
    expect(result.label).toBe("Equip");
    expect(result.description).toContain("target creature you control");
  });

  it("labels an ActivateAbility with its serialized cost", () => {
    const object = makeObject({
      name: "Llanowar Elves",
      abilities: [
        {
          cost: { type: "Tap" },
          description: "{T}: Add {G}.",
          effect: {
            type: "Mana",
            produced: { type: "Fixed", colors: ["Green"] },
          },
        } as unknown as GameObject["abilities"][number],
      ],
    });
    const action: GameAction = {
      type: "ActivateAbility",
      data: { source_id: 1, ability_index: 0 },
    };
    const result = abilityChoiceLabel(action, object);
    // Mana abilities surface the produced symbol, not the tap cost.
    expect(result.label).toBe("Add {G}");
  });

  it("labels an ActivateAbility that adds one mana of any color", () => {
    const object = makeObject({
      name: "Holdout Settlement",
      abilities: [
        {
          cost: {
            type: "Composite",
            costs: [
              { type: "Tap" },
              { type: "TapCreatures", count: 1 },
            ],
          },
          description: "{T}, Tap an untapped creature you control: Add one mana of any color.",
          effect: {
            type: "Mana",
            produced: {
              type: "AnyOneColor",
              count: { type: "Fixed", value: 1 },
              color_options: ["White", "Blue", "Black", "Red", "Green"],
            },
          },
        } as unknown as GameObject["abilities"][number],
      ],
    });
    const action: GameAction = {
      type: "ActivateAbility",
      data: { source_id: 1, ability_index: 0 },
    };

    expect(abilityChoiceLabel(action, object).label).toBe("Add one mana of any color");
  });

  it("labels an ActivateAbility that adds multiple mana of any one color", () => {
    const object = makeObject({
      name: "Gilded Lotus",
      abilities: [
        {
          cost: { type: "Tap" },
          description: "{T}: Add three mana of any one color.",
          effect: {
            type: "Mana",
            produced: {
              type: "AnyOneColor",
              count: { type: "Fixed", value: 3 },
              color_options: ["White", "Blue", "Black", "Red", "Green"],
            },
          },
        } as unknown as GameObject["abilities"][number],
      ],
    });
    const action: GameAction = {
      type: "ActivateAbility",
      data: { source_id: 1, ability_index: 0 },
    };

    expect(abilityChoiceLabel(action, object).label).toBe("Add 3 mana of any one color");
  });

  it("labels a non-mana ActivateAbility with its formatted cost", () => {
    const object = makeObject({
      name: "Quicksilver Dagger",
      abilities: [
        {
          cost: { type: "Tap" },
          description: "{T}: Draw a card.",
          effect: { type: "Draw" },
        } as unknown as GameObject["abilities"][number],
      ],
    });
    const action: GameAction = {
      type: "ActivateAbility",
      data: { source_id: 1, ability_index: 0 },
    };
    const result = abilityChoiceLabel(action, object);
    expect(result.label).toBe("{T}");
    expect(result.description).toBe("Draw a card.");
  });
});
