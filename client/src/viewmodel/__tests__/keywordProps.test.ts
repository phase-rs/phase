import { describe, expect, it } from "vitest";

import type { Keyword } from "../../adapter/types";
import payloadWire from "../../test/fixtures/keyword-payload-wire.json";
import { SHARD_ABBREVIATION } from "../costLabel";
import {
  formatKeywordManaCost,
  getKeywordDetail,
  getKeywordDisplayText,
  getKeywordName,
  getKeywordReminderText,
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
    expect(getKeywordName({ Typecycling: { cost: { type: "Cost", shards: ["White"], generic: 0 }, subtype: "Plains" } })).toBe("Plainscycling");
  });
});

describe("getKeywordDetail", () => {
  it("returns null for simple keywords", () => {
    expect(getKeywordDetail("Flying")).toBeNull();
    expect(getKeywordDetail("Haste")).toBeNull();
  });

  it("formats ManaCost params (internally-tagged serde)", () => {
    expect(getKeywordDetail({ Equip: { type: "Cost", shards: ["White"], generic: 2 } })).toBe("{2}{W}");
    expect(getKeywordDetail({ Kicker: { type: "Cost", shards: [], generic: 4 } })).toBe("{4}");
    expect(getKeywordDetail({ Flashback: { type: "Mana", data: { type: "NoCost" } } })).toBe("{0}");
    expect(getKeywordDetail({ Flashback: { type: "Mana", data: { type: "SelfManaCost" } } })).toBe("its mana cost");
  });

  it("formats the mana cost nested in EmergeCost", () => {
    expect(
      getKeywordDetail({
        Emerge: {
          mana_cost: { type: "Cost", shards: ["Black", "Black"], generic: 5 },
          sacrifice_filter: { type: "Typed", type_filters: ["Artifact"] },
        },
      }),
    ).toBe("{5}{B}{B}");
  });

  it("formats u32 params", () => {
    expect(getKeywordDetail({ Dredge: 3 })).toBe("3");
    expect(getKeywordDetail({ Annihilator: 2 })).toBe("2");
  });

  it("formats Crew power from the struct variant", () => {
    expect(
      getKeywordDetail({
        Crew: { power: 4, once_per_turn: { type: "Unlimited" } },
      } as unknown as Keyword),
    ).toBe("4");
  });

  it("formats quantity keyword params", () => {
    expect(getKeywordDetail({ Firebending: 2 })).toBe("2");
    expect(getKeywordDetail({ Firebending: { type: "Fixed", value: 3 } })).toBe("3");
    expect(getKeywordDetail({ Firebending: { type: "Ref", qty: "SelfPower" } })).toBe("X");
    expect(getKeywordDetail({ Mobilize: 2 })).toBe("2");
    expect(getKeywordDetail({ Mobilize: { type: "Fixed", value: 4 } })).toBe("4");
    expect(getKeywordDetail({ Mobilize: { type: "Ref", qty: "SelfPower" } })).toBe("X");
  });

  it("formats Protection variants", () => {
    expect(getKeywordDetail({ Protection: { Color: "Black" } })).toBe("from black");
    expect(getKeywordDetail({ Protection: "Multicolored" })).toBe("from multicolored");
    expect(getKeywordDetail({ Protection: "ChosenColor" })).toBe("from chosen color");
    expect(getKeywordDetail({ Protection: { CardType: "Instant" } })).toBe("from instants");
    expect(getKeywordDetail({ Protection: { Quality: "Dragons" } })).toBe("from Dragons");
  });

  it("formats Ward variants (adjacently-tagged serde)", () => {
    expect(getKeywordDetail({ Ward: { type: "Mana", data: { type: "Cost", shards: [], generic: 2 } } })).toBe("{2}");
    expect(getKeywordDetail({ Ward: { type: "PayLife", data: 3 } })).toBe("pay 3 life");
    expect(getKeywordDetail({ Ward: { type: "DiscardCard" } })).toBe("discard a card");
    expect(getKeywordDetail({ Ward: { type: "Sacrifice", data: { count: 1, filter: { type: "Any" } } } })).toBe("sacrifice a permanent");
    expect(getKeywordDetail({ Ward: { type: "Sacrifice", data: { count: 2, filter: { type: "Any" } } } })).toBe("sacrifice 2 permanents");
    expect(getKeywordDetail({ Ward: { type: "Sacrifice", data: { count: 1, filter: { type: "Typed", type_filters: ["Creature"] } } } })).toBe("");
    expect(getKeywordDetail({ Ward: { type: "Waterbend", data: { type: "Cost", shards: [], generic: 4 } } })).toBe("waterbend {4}");
  });

  it("formats EtbCounter", () => {
    expect(getKeywordDetail({ EtbCounter: { counter_type: "P1P1", count: 3 } })).toBe("enters with 3 +1/+1 counters");
    expect(getKeywordDetail({ EtbCounter: { counter_type: "lore", count: 1 } })).toBe("enters with 1 lore counter");
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
    expect(getKeywordDisplayText({ Equip: { type: "Cost", shards: [], generic: 3 } })).toBe("Equip {3}");
    expect(getKeywordDisplayText({ Protection: { Color: "Red" } })).toBe("Protection from red");
    expect(
      getKeywordDisplayText({
        Crew: { power: 3, once_per_turn: { type: "Unlimited" } },
      } as unknown as Keyword),
    ).toBe("Crew 3");
    expect(getKeywordDisplayText({ Firebending: { type: "Fixed", value: 2 } })).toBe("Firebending 2");
  });

  it("returns just name for simple keywords", () => {
    expect(getKeywordDisplayText("Flying")).toBe("Flying");
    expect(getKeywordDisplayText("FirstStrike")).toBe("First Strike");
  });
});

describe("getKeywordReminderText", () => {
  it("returns reminder text for simple keywords", () => {
    expect(getKeywordReminderText("Flying")).toContain("creatures with flying or reach");
  });

  it("returns reminder text by keyword name for parameterized keywords", () => {
    expect(getKeywordReminderText({ Ward: { type: "Mana", data: { type: "Cost", shards: [], generic: 2 } } })).toContain("ward cost");
    expect(getKeywordReminderText({ Protection: { Color: "Red" } })).toContain("stated quality");
    expect(
      getKeywordReminderText({
        Crew: { power: 3, once_per_turn: { type: "Unlimited" } },
      } as unknown as Keyword),
    ).toContain("crew value");
  });

  it("returns null when no reminder text is defined", () => {
    expect(getKeywordReminderText({ Unknown: "CustomAbility" })).toBeNull();
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
    const current: Keyword = { Ward: { type: "Mana", data: { type: "Cost", shards: [], generic: 2 } } };
    const base: Keyword[] = [{ Ward: { type: "Mana", data: { type: "Cost", shards: [], generic: 1 } } }];
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
    expect(formatKeywordManaCost({ type: "Cost", shards: [], generic: 3 })).toBe("{3}");
  });

  it("formats shards-only cost", () => {
    expect(formatKeywordManaCost({ type: "Cost", shards: ["White", "Blue"], generic: 0 })).toBe("{W}{U}");
  });

  it("formats mixed cost", () => {
    expect(formatKeywordManaCost({ type: "Cost", shards: ["Red"], generic: 2 })).toBe("{2}{R}");
  });

  it("formats NoCost", () => {
    expect(formatKeywordManaCost({ type: "NoCost" })).toBe("{0}");
  });

  it("formats the self-referential placeholders", () => {
    expect(formatKeywordManaCost({ type: "SelfManaCost" })).toBe("its mana cost");
    expect(formatKeywordManaCost({ type: "SelfManaValue" })).toBe("its mana value");
    expect(formatKeywordManaCost({ type: "SelfManaCostReduced", reduction: 2 })).toBe(
      "its mana cost reduced by {2}",
    );
  });

  it("formats hybrid shards", () => {
    expect(formatKeywordManaCost({ type: "Cost", shards: ["WhiteBlue"], generic: 0 })).toBe("{W/U}");
  });
});

/**
 * Driven by `keyword-payload-wire.json`, which the engine writes from its own
 * serializer (`keyword_payload_wire_golden_matches_the_client_fixture` in
 * crates/engine/src/types/keywords.rs fails when it drifts). Hand-written
 * payloads here once used a shape the engine never emits.
 */
describe("keyword detail over the engine's keyword payload golden", () => {
  type CostPayload = { type: "Cost"; shards: string[]; generic: number };
  const wire = payloadWire as unknown as {
    samples: Keyword[];
    parsed_ward: Record<string, Keyword>;
  };
  const samples = wire.samples;
  const keyOf = (kw: Keyword) => Object.keys(kw)[0];
  const payloadOf = (kw: Keyword) => (kw as Record<string, unknown>)[keyOf(kw)];

  function manaCostsIn(node: unknown): CostPayload[] {
    if (Array.isArray(node)) return node.flatMap(manaCostsIn);
    if (!node || typeof node !== "object") return [];
    const obj = node as Record<string, unknown>;
    if (obj.type === "Cost" && Array.isArray(obj.shards) && typeof obj.generic === "number") {
      return [obj as CostPayload];
    }
    return Object.values(obj).flatMap(manaCostsIn);
  }

  const pips = (c: CostPayload) =>
    (c.generic ? `{${c.generic}}` : "") + c.shards.map((s) => `{${SHARD_ABBREVIATION[s]}}`).join("");

  const manaBearing = samples.filter((kw) => manaCostsIn(payloadOf(kw)).length > 0);

  it("reaches every mana-cost-bearing Keyword variant", () => {
    // 41 bare ManaCost + 9 Mana/NonMana wrappers + 9 structs with a cost field
    // + Disguise + Ward + 2 AbilityCost keywords.
    expect(new Set(manaBearing.map(keyOf)).size).toBe(63);
  });

  it("renders every sample's mana cost in its detail", () => {
    const failures = manaBearing.flatMap((kw) => {
      const detail = getKeywordDetail(kw) ?? "";
      return manaCostsIn(payloadOf(kw))
        .map(pips)
        .filter((p) => !detail.includes(p))
        .map((p) => `${keyOf(kw)}: ${JSON.stringify(detail)} lacks ${p}`);
    });
    expect(failures).toEqual([]);
  });

  it("renders the count-bearing and compound forms as printed", () => {
    const detailOf = (key: string, pick: (kw: Keyword) => boolean = () => true) => {
      const kw = samples.find((s) => keyOf(s) === key && pick(s));
      expect(kw, key).toBeDefined();
      return getKeywordDisplayText(kw!);
    };
    const isType = (type: string) => (kw: Keyword) =>
      (payloadOf(kw) as { type?: string }).type === type;
    expect(detailOf("Suspend")).toBe("Suspend 2—{1}{R}");
    expect(detailOf("Awaken")).toBe("Awaken 4—{5}{W}{W}{W}");
    expect(detailOf("Reinforce")).toBe("Reinforce 3—{2}{G}");
    expect(detailOf("Impending")).toBe("Impending 4—{2}{W}");
    expect(detailOf("Splice")).toBe("Splice onto Arcane {1}{U}");
    expect(detailOf("Prototype")).toBe("Prototype {1}{U} — 2/3");
    expect(detailOf("Ward", isType("Compound"))).toBe("Ward {2}, pay 2 life");
    expect(detailOf("Ward", isType("Waterbend"))).toBe("Ward waterbend {4}");
    expect(detailOf("Blitz", isType("NonMana"))).toBe("Blitz Pay 3 life");
    expect(detailOf("Blitz", isType("Mana"))).toBe("Blitz {2}{R}");
    expect(detailOf("Foretell", isType("SelfManaCostReduced"))).toBe(
      "Foretell its mana cost reduced by {2}",
    );
  });

  it("omits compound Ward detail when a filtered sacrifice leg cannot render", () => {
    expect(getKeywordDisplayText({
      Ward: {
        type: "Compound",
        data: [
          { type: "Mana", data: { type: "Cost", shards: [], generic: 2 } },
          { type: "Sacrifice", data: { count: 1, filter: { type: "Typed", type_filters: ["Creature"] } } },
        ],
      },
    })).toBe("Ward");
  });

  // CR 702.21a: every leg of a compound ward is paid, so the detail is all the
  // legs or none of them. Oracle text → engine parser → serde → this formatter.
  it("renders a parsed compound ward only when every leg renders", () => {
    const parsed = (oracle: string) => {
      const kw = wire.parsed_ward[oracle];
      expect(kw, oracle).toBeDefined();
      return getKeywordDisplayText(kw);
    };
    expect(Object.keys(wire.parsed_ward)).toHaveLength(3);
    expect({
      supported: parsed("Ward—{2}, Pay 2 life."),
      lifeEqualToPower: parsed("Ward—{2}, Pay life equal to this creature's power."),
      playerCounters: parsed("Ward—{1}, Get a poison counter."),
    }).toEqual({
      supported: "Ward {2}, pay 2 life",
      lifeEqualToPower: "Ward",
      playerCounters: "Ward",
    });
  });
});

describe("keyword AbilityCost detail", () => {
  const upkeep = (costs: unknown[]): Keyword => ({
    CumulativeUpkeep: { type: "Composite", costs },
  });
  const mana = { type: "Mana", cost: { type: "Cost", shards: ["Green"], generic: 1 } };
  const payLife = { type: "PayLife", amount: { type: "Fixed", value: 3 } };
  // Aboroth's cost shape, which has no client rendering.
  const effectCost = { type: "EffectCost", effect: { type: "PutCounter" } };
  const sacrificeLand = {
    type: "Sacrifice",
    target: { type: "Typed", type_filters: ["Land"] },
    count: 1,
  };

  it("joins a composite whose every sub-cost renders", () => {
    expect(getKeywordDetail(upkeep([mana, payLife]))).toBe("{1}{G}, Pay 3 life");
    expect(
      getKeywordDetail({ Flashback: { type: "NonMana", data: { type: "Composite", costs: [mana, payLife] } } }),
    ).toBe("{1}{G}, Pay 3 life");
    expect(
      getKeywordDetail({ Blitz: { type: "NonMana", data: { type: "Composite", costs: [mana, payLife] } } }),
    ).toBe("{1}{G}, Pay 3 life");
  });

  it("shows no detail when any sub-cost cannot render", () => {
    expect({
      upkeep: getKeywordDetail(upkeep([mana, effectCost])),
      flashback: getKeywordDetail({
        Flashback: { type: "NonMana", data: { type: "Composite", costs: [mana, effectCost] } },
      }),
      bare: getKeywordDetail({ CumulativeUpkeep: effectCost }),
    }).toEqual({ upkeep: null, flashback: null, bare: null });
  });

  it("does not show a sacrifice cost without its required subject", () => {
    expect({
      bare: getKeywordDetail({ CumulativeUpkeep: sacrificeLand }),
      composite: getKeywordDetail(upkeep([mana, sacrificeLand])),
      flashback: getKeywordDetail({
        Flashback: { type: "NonMana", data: { type: "OneOf", costs: [mana, sacrificeLand] } },
      }),
    }).toEqual({ bare: null, composite: null, flashback: null });
    expect(getKeywordDisplayText({ CumulativeUpkeep: sacrificeLand })).toBe("Cumulative Upkeep");
  });
});
