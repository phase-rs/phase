import { describe, expect, it } from "vitest";

import type { GameObject, ManaPip } from "../../adapter/types";
import { landManaAvailability } from "../manaAvailability";

function makeLand(overrides: Partial<GameObject>): GameObject {
  return {
    id: 1,
    card_id: 0,
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
    name: "Land",
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Land"], subtypes: [] },
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
    ...overrides,
  };
}

const forest: ManaPip = { type: "Color", data: "Green" };
const wasteland: ManaPip = { type: "Colorless" };
const dual: ManaPip = { type: "OneOfColors", data: ["White", "Blue"] };
const triome: ManaPip = { type: "OneOfColors", data: ["White", "Blue", "Black"] };

describe("landManaAvailability", () => {
  it("counts total and untapped lands", () => {
    const result = landManaAvailability([
      makeLand({ id: 1, available_mana_pips: [forest] }),
      makeLand({ id: 2, tapped: true, available_mana_pips: [forest] }),
      makeLand({ id: 3, available_mana_pips: [forest] }),
    ]);
    expect(result.total).toBe(3);
    expect(result.untapped).toBe(2);
    expect(result.sources).toHaveLength(3);
  });

  it("maps a single color to its shard, untapped", () => {
    const [source] = landManaAvailability([
      makeLand({ available_mana_pips: [forest] }),
    ]).sources;
    expect(source).toEqual({ shards: ["G"], rainbow: false, tapped: false });
  });

  it("renders a tapped source as dimmed (tapped) without dropping it", () => {
    const [source] = landManaAvailability([
      makeLand({ tapped: true, available_mana_pips: [wasteland] }),
    ]).sources;
    expect(source).toEqual({ shards: ["C"], rainbow: false, tapped: true });
  });

  it("collapses a two-color choice into ONE hybrid pip (no overcounting)", () => {
    const [source] = landManaAvailability([
      makeLand({ available_mana_pips: [dual] }),
    ]).sources;
    // A dual makes W OR U — one hybrid shard, never one W + one U.
    expect(source.shards).toEqual(["W/U"]);
    expect(source.rainbow).toBe(false);
  });

  it("falls back to a rainbow swatch for 3+ color choices (no hybrid symbol)", () => {
    const [source] = landManaAvailability([
      makeLand({ available_mana_pips: [triome] }),
    ]).sources;
    expect(source.shards).toEqual([]);
    expect(source.rainbow).toBe(true);
  });

  it("treats a no-mana land (no pips) as neither lettered nor rainbow", () => {
    // Maze of Ith: a land with no mana ability still counts toward total but
    // renders as a neutral dot, never implying it taps for mana.
    const [source] = landManaAvailability([makeLand({ available_mana_pips: [] })]).sources;
    expect(source).toEqual({ shards: [], rainbow: false, tapped: false });
  });
});
