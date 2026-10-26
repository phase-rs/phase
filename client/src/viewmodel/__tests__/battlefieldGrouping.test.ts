import { describe, expect, it } from "vitest";

import type { GameObject } from "../../adapter/types";
import { groupByName, partitionByType } from "../battlefieldProps";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../test/factories/gameObjectFactory";
import { buildGameState } from "../../test/factories/gameStateFactory";
import { buildPlayerBattlefieldView } from "../gameStateView";

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
    name: "Test Card",
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Artifact"], subtypes: [] },
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

describe("partitionByType", () => {
  it("separates creatures, lands, support permanents, planeswalkers, and other", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] } }),
      makeGameObject({ id: 2, card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] } }),
      makeGameObject({ id: 3, card_types: { supertypes: [], core_types: ["Artifact"], subtypes: [] } }),
      makeGameObject({ id: 4, card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: [] } }),
      makeGameObject({ id: 5, card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Elf"] } }),
      makeGameObject({ id: 6, card_types: { supertypes: [], core_types: ["Planeswalker"], subtypes: [] } }),
      makeGameObject({ id: 7, card_id: 0, card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Treasure"] } }),
      makeGameObject({ id: 8, card_types: { supertypes: [], core_types: ["Battle"], subtypes: [] } }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1, 5]);
    expect(result.lands).toEqual([2]);
    expect(result.support).toEqual([3, 4, 7]);
    expect(result.planeswalkers).toEqual([6]);
    expect(result.other).toEqual([8]);
  });

  it("returns empty arrays for no objects", () => {
    const result = partitionByType([]);

    expect(result.creatures).toEqual([]);
    expect(result.lands).toEqual([]);
    expect(result.support).toEqual([]);
    expect(result.planeswalkers).toEqual([]);
    expect(result.other).toEqual([]);
  });

  it("classifies land-creatures as creatures", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature", "Land"], subtypes: [] } }),
    ];

    const result = partitionByType(objects);
    // Creatures take priority — animated lands should display in the creature zone
    expect(result.creatures).toEqual([1]);
    expect(result.lands).toEqual([]);
    expect(result.support).toEqual([]);
    expect(result.planeswalkers).toEqual([]);
    expect(result.other).toEqual([]);
  });

  it("keeps creature tokens in the creature zone", () => {
    const objects = [
      makeGameObject({
        id: 1,
        card_id: 0,
        card_types: { supertypes: [], core_types: ["Artifact", "Creature"], subtypes: ["Construct"] },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1]);
    expect(result.support).toEqual([]);
  });

  it("excludes attached Equipment from all partition rows", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] } }),
      makeGameObject({
        id: 99,
        attached_to: { type: "Object", data: 1 },
        card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1]);
    expect(result.support).toEqual([]);
    expect(result.lands).toEqual([]);
    expect(result.planeswalkers).toEqual([]);
    expect(result.other).toEqual([]);
  });

  it("excludes attached Aura from all partition rows", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] } }),
      makeGameObject({
        id: 99,
        attached_to: { type: "Object", data: 1 },
        card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura"] },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1]);
    expect(result.support).toEqual([]);
  });

  it("excludes attached token (card_id === 0) from all partition rows", () => {
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] } }),
      makeGameObject({
        id: 99,
        card_id: 0,
        attached_to: { type: "Object", data: 1 },
        card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1]);
    expect(result.support).toEqual([]);
  });

  it("excludes attached Aura subtype permanents from main rows", () => {
    // CR 702.103: Bestowed creatures are Auras as long as attached. Their
    // core_types still includes "Creature" — without the attached_to filter
    // running first, they'd land in the creatures row AND the chip row.
    const objects = [
      makeGameObject({ id: 1, card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] } }),
      makeGameObject({
        id: 99,
        attached_to: { type: "Object", data: 1 },
        card_types: {
          supertypes: [],
          core_types: ["Creature", "Enchantment"],
          subtypes: ["Satyr", "Aura"],
        },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([1]);
    expect(result.support).toEqual([]);
  });

  it("keeps attached non-attachment creatures in the creature row", () => {
    const objects = [
      makeGameObject({
        id: 99,
        attached_to: { type: "Object", data: 1 },
        card_types: {
          supertypes: [],
          core_types: ["Creature"],
          subtypes: ["Human", "Warrior"],
        },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.creatures).toEqual([99]);
  });

  it("preserves player-attached Auras (status quo) in the support row", () => {
    // AttachTarget::Player(PlayerId) is currently lossy at the WASM boundary —
    // attached_to flattens to null for Curses. Until that's fixed, they must
    // still render somewhere visible. Documented limitation.
    const objects = [
      makeGameObject({
        id: 99,
        attached_to: null,
        card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura", "Curse"] },
      }),
    ];

    const result = partitionByType(objects);

    expect(result.support).toEqual([99]);
  });
});

describe("groupByName", () => {
  it("never folds a melded permanent into a same-named group", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Brisela, Voice of Nightmares" }),
      makeGameObject({ id: 2, name: "Brisela, Voice of Nightmares", merge_kind: "Meld", merged_components: [2, 3] }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.ids.includes(2))).toMatchObject({ ids: [2], count: 1 });
    expect(groups.find((g) => g.ids.includes(2))?.representative.isMelded).toBe(true);
    expect(groups.find((g) => g.ids.includes(1))?.representative.isMelded).toBe(false);
  });

  it("stacks matching permanents by name and tapped state", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Forest" }),
      makeGameObject({ id: 2, name: "Forest" }),
      makeGameObject({ id: 3, name: "Mountain" }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
    expect(groups[0]).toMatchObject({ name: "Forest", ids: [1, 2], count: 2 });
    expect(groups[1]).toMatchObject({ name: "Mountain", ids: [3], count: 1 });
  });

  it("separates tapped and untapped copies into different groups", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Forest", tapped: false }),
      makeGameObject({ id: 2, name: "Forest", tapped: true }),
      makeGameObject({ id: 3, name: "Forest", tapped: false }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
    const untapped = groups.find((g) => !g.representative.tapped);
    const tapped = groups.find((g) => g.representative.tapped);
    expect(untapped).toMatchObject({ count: 2, ids: [1, 3] });
    expect(tapped).toMatchObject({ count: 1, ids: [2] });
  });

  it("only attachments prevent grouping — identical-counter copies stack together", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Grizzly Bears", counters: { Plus1Plus1: 1 } }),
      makeGameObject({ id: 2, name: "Grizzly Bears", counters: { Plus1Plus1: 1 } }),
      makeGameObject({ id: 3, name: "Grizzly Bears", attachments: [99] }),
    ];
    // The group identity reads the ENGINE's rows, so the fixture supplies them; `obj.counters` is
    // kept in sync only so the object stays a plausible engine snapshot.
    const counterDisplay = {
      "1": { pills: [{ counter: "Plus1Plus1", count: 1 }] },
      "2": { pills: [{ counter: "Plus1Plus1", count: 1 }] },
    };

    const groups = groupByName(objects, undefined, undefined, counterDisplay, false);

    // Two copies with identical counters stack; the one with an attachment is solo
    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 2]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([3]);
  });

  it("renders the ring-bearer solo even among identical same-named copies (issue #721)", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Orc Army" }),
      makeGameObject({ id: 2, name: "Orc Army" }),
      makeGameObject({ id: 3, name: "Orc Army" }),
    ];

    const groups = groupByName(objects, new Set([2]), undefined, undefined, false);

    // The ring-bearer (id 2) never gets hidden behind a non-bearer
    // representative in a collapsed group — it always has its own entry so
    // PermanentCard's ring-bearer badge is reachable.
    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("separates copies with different counter amounts", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Grizzly Bears", counters: { Plus1Plus1: 1 } }),
      makeGameObject({ id: 2, name: "Grizzly Bears", counters: { Plus1Plus1: 2 } }),
      makeGameObject({ id: 3, name: "Grizzly Bears", counters: { Plus1Plus1: 1 } }),
    ];
    const counterDisplay = {
      "1": { pills: [{ counter: "Plus1Plus1", count: 1 }] },
      "2": { pills: [{ counter: "Plus1Plus1", count: 2 }] },
      "3": { pills: [{ counter: "Plus1Plus1", count: 1 }] },
    };

    const groups = groupByName(objects, undefined, undefined, counterDisplay, false);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("separates copies with different damage marked", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Grizzly Bears", power: 2, toughness: 2 }),
      makeGameObject({ id: 2, name: "Grizzly Bears", power: 2, toughness: 2, damage_marked: 1 }),
      makeGameObject({ id: 3, name: "Grizzly Bears", power: 2, toughness: 2 }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("separates copies with different power/toughness (pump effects)", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Grizzly Bears", power: 2, toughness: 2 }),
      makeGameObject({ id: 2, name: "Grizzly Bears", power: 4, toughness: 4 }),
      makeGameObject({ id: 3, name: "Grizzly Bears", power: 2, toughness: 2 }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("separates copies with different keywords (temporary keyword grants)", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Llanowar Elves", keywords: [] }),
      makeGameObject({ id: 2, name: "Llanowar Elves", keywords: ["Flying"] }),
      makeGameObject({ id: 3, name: "Llanowar Elves", keywords: [] }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("separates copies with different parameterized keywords (e.g. ward {2} vs ward {3})", () => {
    const ward2 = { type: "Ward", data: { type: "Generic", amount: 2 } };
    const ward3 = { type: "Ward", data: { type: "Generic", amount: 3 } };
    const objects = [
      makeGameObject({ id: 1, name: "Spectral Sailor", keywords: [ward2] }),
      makeGameObject({ id: 2, name: "Spectral Sailor", keywords: [ward3] }),
      makeGameObject({ id: 3, name: "Spectral Sailor", keywords: [ward2] }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("separates copies with different colors (color-changing effects)", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Grizzly Bears", color: ["Green"] }),
      makeGameObject({ id: 2, name: "Grizzly Bears", color: ["Blue"] }),
      makeGameObject({ id: 3, name: "Grizzly Bears", color: ["Green"] }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
    expect(groups.find((g) => g.count === 2)?.ids).toEqual([1, 3]);
    expect(groups.find((g) => g.count === 1)?.ids).toEqual([2]);
  });

  it("preserves name and representative for each permanent", () => {
    const objects = [
      makeGameObject({ id: 5, name: "Forest" }),
      makeGameObject({ id: 9, name: "Mountain" }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups[0].name).toBe("Forest");
    expect(groups[0].representative.id).toBe(5);
    expect(groups[1].name).toBe("Mountain");
    expect(groups[1].representative.id).toBe(9);
  });

  it("keeps visually distinct tokens with the same name in separate groups", () => {
    const attackPest = makeGameObject({
      id: 1,
      card_id: 0,
      name: "Pest",
      power: 1,
      toughness: 1,
      display_source: "Token",
      token_rules_text: "Whenever this token attacks, you gain 1 life.",
      token_image_ref: {
        preset_id: "00a0801d-0212-5890-8957-3cde30f382f9",
        scryfall_id: "ba854032-6ad2-4654-990a-64006e7f92fd",
      },
      card_types: {
        supertypes: [],
        core_types: ["Creature"],
        subtypes: ["Pest"],
      },
      color: ["Black", "Green"],
    });
    const diesPest = makeGameObject({
      id: 2,
      card_id: 0,
      name: "Pest",
      power: 1,
      toughness: 1,
      display_source: "Token",
      token_rules_text: "When this creature dies, you gain 1 life.",
      token_image_ref: {
        preset_id: "14c28cbd-1740-5c17-98ea-4aea094067f1",
        scryfall_id: "2b613822-b9c2-439e-9533-a91bed12b5e9",
      },
      card_types: {
        supertypes: [],
        core_types: ["Creature"],
        subtypes: ["Pest"],
      },
      color: ["Black", "Green"],
    });

    const groups = groupByName([attackPest, diesPest], undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
    expect(groups.map((g) => g.count).sort()).toEqual([1, 1]);
  });

  it("separates tokens that differ only in token_rules_text", () => {
    const sharedImage = {
      preset_id: "14c28cbd-1740-5c17-98ea-4aea094067f1",
      scryfall_id: "2b613822-b9c2-439e-9533-a91bed12b5e9",
    };
    const objects = [
      makeGameObject({
        id: 1,
        card_id: 0,
        name: "Pest",
        power: 1,
        toughness: 1,
        is_token: true,
        display_source: "Token",
        token_rules_text: "Whenever this token attacks, you gain 1 life.",
        token_image_ref: sharedImage,
        card_types: {
          supertypes: [],
          core_types: ["Creature"],
          subtypes: ["Pest"],
        },
        color: ["Black", "Green"],
      }),
      makeGameObject({
        id: 2,
        card_id: 0,
        name: "Pest",
        power: 1,
        toughness: 1,
        is_token: true,
        display_source: "Token",
        token_rules_text: "When this creature dies, you gain 1 life.",
        token_image_ref: sharedImage,
        card_types: {
          supertypes: [],
          core_types: ["Creature"],
          subtypes: ["Pest"],
        },
        color: ["Black", "Green"],
      }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
  });

  it("separates tokens that differ only in token_image_ref.preset_id", () => {
    const sharedRules = "When this creature dies, you gain 1 life.";
    const objects = [
      makeGameObject({
        id: 1,
        card_id: 0,
        name: "Pest",
        power: 1,
        toughness: 1,
        is_token: true,
        display_source: "Token",
        token_rules_text: sharedRules,
        token_image_ref: {
          preset_id: "00a0801d-0212-5890-8957-3cde30f382f9",
          scryfall_id: "ba854032-6ad2-4654-990a-64006e7f92fd",
        },
        card_types: {
          supertypes: [],
          core_types: ["Creature"],
          subtypes: ["Pest"],
        },
        color: ["Black", "Green"],
      }),
      makeGameObject({
        id: 2,
        card_id: 0,
        name: "Pest",
        power: 1,
        toughness: 1,
        is_token: true,
        display_source: "Token",
        token_rules_text: sharedRules,
        token_image_ref: {
          preset_id: "14c28cbd-1740-5c17-98ea-4aea094067f1",
          scryfall_id: "2b613822-b9c2-439e-9533-a91bed12b5e9",
        },
        card_types: {
          supertypes: [],
          core_types: ["Creature"],
          subtypes: ["Pest"],
        },
        color: ["Black", "Green"],
      }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
  });

  it("keeps tokens separate from non-token cards with the same name", () => {
    const objects = [
      makeGameObject({ id: 1, name: "Soldier", power: 1, toughness: 1 }),
      makeGameObject({
        id: 2,
        card_id: 0,
        name: "Soldier",
        power: 1,
        toughness: 1,
        is_token: true,
        display_source: "Token",
      }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
  });

  it("keeps face-down permanents separate in battlefield order", () => {
    const objects = [
      makeGameObject({
        id: 54,
        name: "Hidden Sorcery",
        face_down: true,
        power: 2,
        toughness: 2,
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
      }),
      makeGameObject({
        id: 55,
        name: "Hidden Instant",
        face_down: true,
        power: 2,
        toughness: 2,
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
      }),
    ];

    const groups = groupByName(objects, undefined, undefined, undefined, false);

    expect(groups).toHaveLength(2);
    expect(groups.map((group) => group.ids)).toEqual([[54], [55]]);
    expect(groups.map((group) => group.name)).toEqual(["Face-down card", "Face-down card"]);
  });

  // DESIGN STEP 4 (∞-pile): groupByName marks a group isUnboundedPile iff EVERY
  // member is in the engine-authored pile set (CR 732.2a).
  it("marks a group isUnboundedPile only when all members are in the pile set", () => {
    const tappedFodder = [
      makeGameObject({
        id: 1,
        tapped: true,
        name: "Saproling",
        power: 1,
        toughness: 1,
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Saproling"] },
      }),
      makeGameObject({
        id: 2,
        tapped: true,
        name: "Saproling",
        power: 1,
        toughness: 1,
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Saproling"] },
      }),
    ];
    const pile = new Set([1, 2]);

    // Both tapped Saprolings are pile members → the group is ∞.
    const infinite = groupByName(tappedFodder, undefined, pile, undefined, false);
    expect(infinite).toHaveLength(1);
    expect(infinite[0].isUnboundedPile).toBe(true);

    // An UNtapped Saproling groups separately (groupKey splits on tapped) and is
    // NOT in the pile → its group is ×N, not ∞.
    const untapped = makeGameObject({
      id: 3,
      tapped: false,
      name: "Saproling",
      power: 1,
      toughness: 1,
      card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Saproling"] },
    });
    const mixed = groupByName([...tappedFodder, untapped], undefined, pile, undefined, false);
    const untappedGroup = mixed.find((g) => g.ids.includes(3));
    expect(untappedGroup?.isUnboundedPile).toBe(false);

    // Empty pile set → no group is ∞ (the dominant no-loop case; also the default).
    expect(groupByName(tappedFodder, undefined, undefined, undefined, false)[0].isUnboundedPile).toBe(false);
    expect(groupByName(tappedFodder, undefined, new Set(), undefined, false)[0].isUnboundedPile).toBe(false);
  });

  // CR 122.1 + CR 732.2a: the group identity keys on the engine's RENDERED counter rows, so a
  // collapsed group's representative can never speak for a member that renders differently. This
  // is the live `GroupedPermanent` representative bug — the collapsed branch subscribes the
  // counter projection for `ids[0]` alone.
  it("splits a group whose members disagree on their counter rows, and only then", () => {
    const twins = () => [
      makeGameObject({ id: 1, name: "Pentad Prism", counters: { charge: 4 } }),
      makeGameObject({ id: 2, name: "Pentad Prism", counters: { charge: 4 } }),
    ];

    // NEGATIVE — THE FIX. Byte-identical counter maps; only id 1 is `∞`-annotated. `magnitude` is
    // absent on id 2's row exactly as the engine omits the serde default.
    const divergent = groupByName(twins(), undefined, undefined, {
      "1": { pills: [{ counter: "charge", count: 4, magnitude: "Unbounded" }] },
      "2": { pills: [{ counter: "charge", count: 4 }] },
    }, false);
    expect(divergent).toHaveLength(2);
    expect(divergent.find((g) => g.ids.includes(1))?.ids).toEqual([1]);

    // POSITIVE — THE REACH-GUARD. The SAME two objects with the SAME entry collapse into one
    // group, which is what proves the split above is caused by the ∞ divergence and by nothing
    // else (a groupKey that split on object id would pass the negative arm vacuously).
    const identical = groupByName(twins(), undefined, undefined, {
      "1": { pills: [{ counter: "charge", count: 4, magnitude: "Unbounded" }] },
      "2": { pills: [{ counter: "charge", count: 4, magnitude: "Unbounded" }] },
    }, false);
    expect(identical).toHaveLength(1);
    expect(identical[0].ids).toEqual([1, 2]);

    // FREE ARM 1 — objects with no entry at all still collapse, i.e. the new fragment does not
    // accidentally key on identity.
    expect(groupByName(twins(), undefined, undefined, {}, false)).toHaveLength(1);

    // FREE ARM 2 — the DE-split direction (behavior change #5). The engine drops zero-count
    // finite rows (CR 122.1), so a `{charge: 0}` permanent and a counterless one publish the same
    // (empty) row set and now render identically. Reading `obj.counters` here instead of the
    // projection splits them, as the head revision did.
    const zeroVsAbsent = [
      makeGameObject({ id: 1, name: "Pentad Prism", counters: { charge: 0 } }),
      makeGameObject({ id: 2, name: "Pentad Prism", counters: {} }),
    ];
    expect(groupByName(zeroVsAbsent, undefined, undefined, {}, false)).toHaveLength(1);
  });
});

describe("owner-printing battlefield identity", () => {
  const a = { set_code: "own", collector_number: "01" };
  const b = { set_code: "own", collector_number: "02" };
  const card = (id: number, printing?: GameObject["printing"]) =>
    buildGameObjectWithCoreTypes(["Creature"], { id, name: "Alpha", printing,
      printed_ref: { oracle_id: "oracle-a", face_name: "Alpha" } });
  const ids = (objects: GameObject[], enabled: boolean) =>
    groupByName(objects, undefined, undefined, undefined, enabled).map((group) => group.ids);

  it("compares both delivered fields by value, with null and omission equal", () => {
    const objects = [
      card(1, a),
      card(2, { collector_number: "01", set_code: "own" }),
      card(3, b),
      card(4, { set_code: "other", collector_number: "01" }),
      card(5, null),
      card(6),
    ];
    expect(ids(objects, true)).toEqual([[1, 2], [3], [4], [5, 6]]);
    expect(ids(objects, false)).toEqual([[1, 2, 3, 4, 5, 6]]);
  });

  it.each(["different", "face", "missing", "equal", "legacy"] as const)(
    "groups the effective selected authority, not just the pair: %s", (authority) => {
      const first = card(1, a);
      const second = card(2, a);
      if (authority === "different") second.printed_ref = { oracle_id: "oracle-b", face_name: "Alpha" };
      if (authority === "face") second.printed_ref = { oracle_id: "oracle-a", face_name: "Reverse" };
      if (authority === "missing" || authority === "legacy") second.printed_ref = null;
      if (authority === "legacy") first.printed_ref = null;
      for (const objects of [[first, second], [second, first]]) {
        const order = objects.map((object) => object.id);
        expect(ids(objects, true)).toEqual(["different", "face", "missing"].includes(authority)
          ? order.map((id) => [id]) : [order]);
        expect(ids(objects, false)).toEqual([order]);
      }
    },
  );

  it("coalesces transformed legacy names but separates identified emblem own art", () => {
    const objects = [card(1, a), card(2, a)];
    for (const [index, object] of objects.entries()) {
      object.printed_ref = null;
      object.transformed = true;
      object.back_face = { ...objects[index], name: index === 0 ? "Front A" : "Front B" };
    }
    for (const order of [objects, [...objects].reverse()]) {
      expect(ids(order, true)).toEqual([order.map((object) => object.id)]);
      expect(ids(order, false)).toEqual([order.map((object) => object.id)]);
    }
    for (const object of objects) {
      object.transformed = false;
      object.is_emblem = true;
      object.emblem_source = { name: "Creator", printed_ref: { oracle_id: "creator", face_name: "Creator" }, printing: b };
      object.printed_ref = { oracle_id: `own-${object.id}`, face_name: "Alpha" };
    }
    for (const order of [objects, [...objects].reverse()]) {
      expect(ids(order, true)).toEqual(order.map((object) => [object.id]));
      expect(ids(order, false)).toEqual([order.map((object) => object.id)]);
    }
  });

  it.each([
    [{ set_code: "OWN", collector_number: "01" }, a],
    [{ set_code: " own", collector_number: "01" }, a],
    [{ set_code: "own", collector_number: "1" }, a],
    [{ set_code: "own", collector_number: "01 " }, a],
    [{ set_code: "a|b", collector_number: "c" }, { set_code: "a", collector_number: "b|c" }],
    [{ set_code: "", collector_number: "" }, null],
    [{ set_code: "invalid", collector_number: "x" }, { set_code: "foreign", collector_number: "x" }],
  ])("never normalizes or admits printing identity (%j vs %j)", (left, right) => {
    const objects = [card(1, left), card(2, { ...left }), card(3, right)];
    expect(ids(objects, true)).toEqual([[1, 2], [3]]);
    expect(ids(objects, false)).toEqual([[1, 2, 3]]);
  });

  it.each([false, true])("keeps concealed singletons and face-up absent twins (%s)", (enabled) => {
    const objects = [
      { ...card(7), face_down: true },
      { ...card(8, null), face_down: true },
      card(9),
      card(10, null),
    ];
    expect(ids(objects, enabled)).toEqual([[7], [8], [9, 10]]);
    expect(ids([], enabled)).toEqual([]);
  });

  it("preserves insertion order, representatives and every-member infinity after splitting", () => {
    const objects = [card(5, a), card(2, b), card(8, a), card(3, b), card(9, null), card(10)];
    const grouped = (enabled: boolean) =>
      groupByName(objects, undefined, new Set([5, 8, 2]), undefined, enabled)
        .map((group) => [group.ids, group.count, group.representative.id, group.isUnboundedPile]);
    expect(grouped(false)).toEqual([[[5, 2, 8, 3, 9, 10], 6, 5, false]]);
    expect(grouped(true)).toEqual([
      [[5, 8], 2, 5, true], [[2, 3], 2, 2, false], [[9, 10], 2, 9, false],
    ]);
    expect(groupByName([card(2, b)], undefined, new Set([2]), undefined, true)[0].isUnboundedPile).toBe(true);
  });

  it.each([false, true])("retains legacy nonprinting boundaries and first-member order (%s)", (enabled) => {
    const differences: Partial<GameObject>[] = [
      { tapped: true }, { power: 3 }, { damage_marked: 1 }, { is_commander: true },
      { is_token: true }, { token_rules_text: "Distinct delivered text" },
      { keywords: ["Flying"] }, { color: ["Red"] }, { attachments: [99] },
      { has_summoning_sickness: true }, { transformed: true }, { flipped: true },
      { merge_kind: "Meld", merged_components: [2, 3] },
    ];
    for (const difference of differences) {
      const objects = [card(5, a), { ...card(2, a), ...difference }, card(8, { ...a })];
      const groups = groupByName(objects, undefined, undefined, undefined, enabled);
      expect(groups.map((group) => [group.ids, group.count, group.representative.id]))
        .toEqual([[[5, 8], 2, 5], [[2], 1, 2]]);
    }
    const objects = [card(5, a), card(2, a), card(8, a)];
    expect(groupByName(objects, new Set([2]), undefined, undefined, enabled)
      .map((group) => group.ids)).toEqual([[5, 8], [2]]);
    expect(groupByName(objects, undefined, undefined, {
      "2": { pills: [{ counter: "charge", count: 1 }] },
    }, enabled).map((group) => group.ids)).toEqual([[5, 8], [2]]);
  });

  it("threads the viewer choice through all five builder rows without selecting by owner", () => {
    const types = ["Creature", "Land", "Artifact", "Planeswalker", "Battle"] as const;
    const objects = types.flatMap((type, index) =>
      [a, { ...a }, b].map((printing, member) =>
        buildGameObjectWithCoreTypes([type], {
          id: index * 10 + member + 1, name: type, printing, owner: 1, controller: 0,
          printed_ref: { oracle_id: "oracle-a", face_name: type },
        })));
    const state = buildGameState({ objects: buildObjectMap(...objects), battlefield: objects.map((object) => object.id) });
    for (const enabled of [false, true]) {
      const view = buildPlayerBattlefieldView(state, 0, enabled);
      for (const [index, row] of ["creatures", "lands", "support", "planeswalkers", "other"].entries()) {
        expect(view[row as keyof typeof view].map((group) => group.ids)).toEqual(enabled
          ? [[index * 10 + 1, index * 10 + 2], [index * 10 + 3]]
          : [[index * 10 + 1, index * 10 + 2, index * 10 + 3]]);
      }
      expect(buildPlayerBattlefieldView(state, 1, enabled)).toEqual({
        creatures: [], lands: [], support: [], planeswalkers: [], other: [],
      });
      expect(buildPlayerBattlefieldView(null, 0, enabled)).toEqual({
        creatures: [], lands: [], support: [], planeswalkers: [], other: [],
      });
    }
  });
});
