import { describe, expect, it } from "vitest";

import type { Keyword } from "../../adapter/types";
import {
  formatKeywordManaCost,
  getKeywordDetail,
  getKeywordDisplayText,
  getKeywordName,
  isGrantedKeyword,
  sortKeywords,
} from "../keywordProps";

describe("getKeywordName", () => {
  it("returns string keywords with PascalCase splitting", () => {
    expect(getKeywordName("Flying")).toBe("Flying");
    expect(getKeywordName("FirstStrike")).toBe("First Strike");
    expect(getKeywordName("DoubleStrike")).toBe("Double Strike");
    expect(getKeywordName("Deathtouch")).toBe("Deathtouch");
  });

  it("uses name overrides", () => {
    expect(getKeywordName("EtbCounter")).toBe("ETB Counter");
    expect(getKeywordName("LivingWeapon")).toBe("Living Weapon");
    expect(getKeywordName("SplitSecond")).toBe("Split Second");
  });

  it("extracts name from object keywords", () => {
    expect(getKeywordName({ Equip: { type: "Cost", shards: [], generic: 2 } })).toBe("Equip");
    expect(getKeywordName({ Dredge: 3 })).toBe("Dredge");
  });

  it("handles Unknown keyword", () => {
    expect(getKeywordName({ Unknown: "CustomAbility" })).toBe("CustomAbility");
  });

  it("uses variant names for Partner family", () => {
    expect(getKeywordName({ Partner: { type: "Generic" } })).toBe("Partner");
    expect(getKeywordName({ Partner: { type: "With", data: "Shabraz" } })).toBe("Partner");
    expect(getKeywordName({ Partner: { type: "FriendsForever" } })).toBe("Friends Forever");
    expect(getKeywordName({ Partner: { type: "CharacterSelect" } })).toBe("Character Select");
    expect(getKeywordName({ Partner: { type: "DoctorsCompanion" } })).toBe("Doctor's Companion");
    expect(getKeywordName({ Partner: { type: "ChooseABackground" } })).toBe("Choose a Background");
  });

  it("handles Typecycling with subtype", () => {
    expect(getKeywordName({ Typecycling: { cost: { Cost: { shards: ["White"], generic: 0 } }, subtype: "Plains" } })).toBe("Plainscycling");
  });
});

describe("getKeywordDetail", () => {
  it("returns null for simple keywords", () => {
    expect(getKeywordDetail("Flying")).toBeNull();
    expect(getKeywordDetail("Haste")).toBeNull();
  });

  it("formats ManaCost params (externally-tagged serde)", () => {
    expect(getKeywordDetail({ Equip: { Cost: { shards: ["White"], generic: 2 } } })).toBe("{2}{W}");
    expect(getKeywordDetail({ Flashback: "NoCost" })).toBe("{0}");
    expect(getKeywordDetail({ Flashback: "SelfManaCost" })).toBe("its mana cost");
  });

  it("formats u32 params", () => {
    expect(getKeywordDetail({ Dredge: 3 })).toBe("3");
    expect(getKeywordDetail({ Crew: 4 })).toBe("4");
    expect(getKeywordDetail({ Annihilator: 2 })).toBe("2");
  });

  it("formats Protection variants", () => {
    expect(getKeywordDetail({ Protection: { Color: "Black" } })).toBe("from black");
    expect(getKeywordDetail({ Protection: "Multicolored" })).toBe("from multicolored");
    expect(getKeywordDetail({ Protection: "ChosenColor" })).toBe("from chosen color");
    expect(getKeywordDetail({ Protection: { CardType: "Instant" } })).toBe("from instants");
    expect(getKeywordDetail({ Protection: { Quality: "Dragons" } })).toBe("from Dragons");
  });

  it("formats Ward variants (adjacently-tagged serde)", () => {
    expect(getKeywordDetail({ Ward: { type: "Mana", data: { Cost: { shards: [], generic: 2 } } } })).toBe("{2}");
    expect(getKeywordDetail({ Ward: { type: "PayLife", data: 3 } })).toBe("pay 3 life");
    expect(getKeywordDetail({ Ward: { type: "DiscardCard" } })).toBe("discard a card");
    expect(getKeywordDetail({ Ward: { type: "Sacrifice", data: { count: 1, filter: { type: "Any" } } } })).toBe("sacrifice a permanent");
    expect(getKeywordDetail({ Ward: { type: "Sacrifice", data: { count: 2, filter: { type: "Any" } } } })).toBe("sacrifice 2 permanents");
    expect(getKeywordDetail({ Ward: { type: "Waterbend", data: { Cost: { shards: [], generic: 4 } } } })).toBe("waterbend {4}");
  });

  it("formats EtbCounter", () => {
    expect(getKeywordDetail({ EtbCounter: { counter_type: "Plus1Plus1", count: 3 } })).toBe("enters with 3 +1/+1 counters");
    expect(getKeywordDetail({ EtbCounter: { counter_type: "Lore", count: 1 } })).toBe("enters with 1 lore counter");
  });

  it("formats Partner", () => {
    expect(getKeywordDetail({ Partner: { type: "With", data: "Brallin, Skyshark Rider" } })).toBe("with Brallin, Skyshark Rider");
    expect(getKeywordDetail({ Partner: { type: "Generic" } })).toBeNull();
    expect(getKeywordDetail({ Partner: { type: "FriendsForever" } })).toBeNull();
    expect(getKeywordDetail({ Partner: { type: "DoctorsCompanion" } })).toBeNull();
    expect(getKeywordDetail({ Partner: { type: "ChooseABackground" } })).toBeNull();
    expect(getKeywordDetail({ Partner: { type: "CharacterSelect" } })).toBeNull();
  });

  it("returns null for Enchant and Companion", () => {
    expect(getKeywordDetail({ Enchant: { type: "Creature" } })).toBeNull();
    expect(getKeywordDetail({ Companion: { type: "Singleton" } })).toBeNull();
  });
});

describe("getKeywordDisplayText", () => {
  it("combines name and detail", () => {
    expect(getKeywordDisplayText({ Equip: { Cost: { shards: [], generic: 3 } } })).toBe("Equip {3}");
    expect(getKeywordDisplayText({ Protection: { Color: "Red" } })).toBe("Protection from red");
    expect(getKeywordDisplayText({ Crew: 3 })).toBe("Crew 3");
  });

  it("returns just name for simple keywords", () => {
    expect(getKeywordDisplayText("Flying")).toBe("Flying");
    expect(getKeywordDisplayText("FirstStrike")).toBe("First Strike");
  });
});

describe("isGrantedKeyword", () => {
  it("returns true when keyword is not in base", () => {
    expect(isGrantedKeyword("Flying", ["Deathtouch"])).toBe(true);
  });

  it("returns false when keyword is in base", () => {
    expect(isGrantedKeyword("Flying", ["Flying", "Deathtouch"])).toBe(false);
  });

  it("compares by name for parameterized keywords", () => {
    const current: Keyword = { Ward: { type: "Mana", data: { Cost: { shards: [], generic: 2 } } } };
    const base: Keyword[] = [{ Ward: { type: "Mana", data: { Cost: { shards: [], generic: 1 } } } }];
    expect(isGrantedKeyword(current, base)).toBe(false);
  });
});

describe("sortKeywords", () => {
  it("sorts combat keywords first", () => {
    const keywords: Keyword[] = ["Haste", "Deathtouch", "Flying"];
    const sorted = sortKeywords(keywords);
    expect(sorted.map((k) => getKeywordName(k))).toEqual(["Flying", "Deathtouch", "Haste"]);
  });

  it("sorts non-priority keywords alphabetically", () => {
    const keywords: Keyword[] = ["Prowess", "Changeling", "Ascend"];
    const sorted = sortKeywords(keywords);
    expect(sorted.map((k) => getKeywordName(k))).toEqual(["Ascend", "Changeling", "Prowess"]);
  });
});

describe("formatKeywordManaCost", () => {
  it("formats generic-only cost", () => {
    expect(formatKeywordManaCost({ Cost: { shards: [], generic: 3 } })).toBe("{3}");
  });

  it("formats shards-only cost", () => {
    expect(formatKeywordManaCost({ Cost: { shards: ["White", "Blue"], generic: 0 } })).toBe("{W}{U}");
  });

  it("formats mixed cost", () => {
    expect(formatKeywordManaCost({ Cost: { shards: ["Red"], generic: 2 } })).toBe("{2}{R}");
  });

  it("formats NoCost (string variant)", () => {
    expect(formatKeywordManaCost("NoCost")).toBe("{0}");
  });

  it("formats SelfManaCost (string variant)", () => {
    expect(formatKeywordManaCost("SelfManaCost")).toBe("its mana cost");
  });

  it("formats hybrid shards", () => {
    expect(formatKeywordManaCost({ Cost: { shards: ["WhiteBlue"], generic: 0 } })).toBe("{W/U}");
  });
});
