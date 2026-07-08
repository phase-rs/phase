import { describe, expect, it } from "vitest";

import { classifyByColor, classifyByType, groupAccent, groupRank } from "../deckGrouping";
import type { ScryfallCard } from "../../../services/scryfall";

function makeCard(typeLine: string, colorIdentity: string[] = []): ScryfallCard {
  return {
    id: typeLine.toLowerCase(),
    name: typeLine,
    mana_cost: "",
    cmc: 0,
    type_line: typeLine,
    color_identity: colorIdentity,
    legalities: {},
  };
}

describe("classifyByType", () => {
  it("uses land-before-creature precedence for multi-type lines", () => {
    expect(classifyByType(makeCard("Land Creature — Dryad Arbor"))).toBe("Lands");
    expect(classifyByType(makeCard("Artifact Land"))).toBe("Lands");
  });

  it("classifies an artifact creature as a creature (creature precedence)", () => {
    expect(classifyByType(makeCard("Artifact Creature — Construct"))).toBe("Creatures");
  });

  it("classifies the remaining card types", () => {
    expect(classifyByType(makeCard("Planeswalker — Jace"))).toBe("Planeswalkers");
    expect(classifyByType(makeCard("Instant"))).toBe("Instants");
    expect(classifyByType(makeCard("Sorcery"))).toBe("Sorceries");
    expect(classifyByType(makeCard("Artifact — Equipment"))).toBe("Artifacts");
    expect(classifyByType(makeCard("Enchantment — Aura"))).toBe("Enchantments");
    expect(classifyByType(makeCard("Battle — Siege"))).toBe("Battles");
  });

  it("falls back to Other for unknown types and undefined cards", () => {
    expect(classifyByType(makeCard("Conspiracy"))).toBe("Other");
    expect(classifyByType(undefined)).toBe("Other");
  });
});

describe("classifyByColor", () => {
  it("maps a single color identity to its color name", () => {
    expect(classifyByColor(makeCard("Creature", ["W"]))).toBe("White");
    expect(classifyByColor(makeCard("Creature", ["U"]))).toBe("Blue");
    expect(classifyByColor(makeCard("Creature", ["B"]))).toBe("Black");
    expect(classifyByColor(makeCard("Creature", ["R"]))).toBe("Red");
    expect(classifyByColor(makeCard("Creature", ["G"]))).toBe("Green");
  });

  it("treats 2+ colors as Multicolor", () => {
    expect(classifyByColor(makeCard("Creature", ["W", "U"]))).toBe("Multicolor");
  });

  it("treats an empty identity as Colorless", () => {
    expect(classifyByColor(makeCard("Artifact", []))).toBe("Colorless");
  });

  it("classifies lands as Lands regardless of color identity", () => {
    expect(classifyByColor(makeCard("Land", ["G"]))).toBe("Lands");
    expect(classifyByColor(makeCard("Basic Land — Forest", []))).toBe("Lands");
  });

  it("treats an undefined card as Colorless", () => {
    expect(classifyByColor(undefined)).toBe("Colorless");
  });
});

describe("groupRank", () => {
  it("orders type groups with Creatures before Lands before Other", () => {
    const creature = makeCard("Creature");
    const land = makeCard("Basic Land — Plains");
    const other = makeCard("Conspiracy");
    expect(groupRank("type", creature)).toBeLessThan(groupRank("type", land));
    expect(groupRank("type", land)).toBeLessThan(groupRank("type", other));
  });

  it("orders color groups with White before Lands", () => {
    const white = makeCard("Creature", ["W"]);
    const land = makeCard("Land", ["W"]);
    expect(groupRank("color", white)).toBeLessThan(groupRank("color", land));
  });
});

describe("groupAccent", () => {
  it("resolves a raw group key (list view) to its accent", () => {
    expect(groupAccent("White").bar).toBe("bg-amber-200");
    expect(groupAccent("Creatures").text).toBe("text-emerald-300");
  });

  it("resolves a prefixed title sub-path (stack view) by stripping the prefix", () => {
    expect(groupAccent("colorGroup.White")).toEqual(groupAccent("White"));
    expect(groupAccent("group.Lands")).toEqual(groupAccent("Lands"));
  });

  it("falls back to a neutral accent for sideboard/maybeboard and unknown keys", () => {
    const neutral = groupAccent("sideboardGroup");
    expect(neutral.bar).toBe("bg-white/15");
    expect(groupAccent("maybeboardGroup")).toEqual(neutral);
    expect(groupAccent("")).toEqual(neutral);
  });
});
