import { describe, expect, it } from "vitest";

import {
  isCustomFormatRulesShape,
  isFormatConfigShape,
} from "../format-config-shape";
import type { CustomFormatRules, FormatConfig } from "../types";

/**
 * These guards stand between `JSON.parse` and code that reads `.min_players`,
 * `.deck_size.type`, `.custom_rules`, and so on. TypeScript is erased at
 * runtime, so nothing else validates either of the two untrusted ingresses —
 * `localStorage` rehydration and a broker's `PeerInfo`/`JoinTargetInfo` frame.
 */

function builtInConfig(): FormatConfig {
  return {
    format: "Commander",
    starting_life: 40,
    min_players: 2,
    max_players: 6,
    deck_size: { type: "Exactly", data: 100 },
    singleton: true,
    command_zone: true,
    commander_damage_threshold: 21,
    range_of_influence: null,
    team_based: false,
    uses_commander: true,
    supplies_fixed_deck: false,
    sideboard_policy: { type: "Forbidden" },
    default_deck_copy_limit: { type: "UpTo", data: 1 },
    allow_debug_actions: false,
  };
}

function customRules(id = 0): CustomFormatRules {
  return {
    id,
    structural: {
      starting_life: 20,
      min_players: 2,
      max_players: 4,
      deck_size: { type: "Minimum", data: 60 },
      singleton: false,
      command_zone_mode: "Disabled",
      range_of_influence: null,
      team_based: false,
      sideboard_policy: { type: "Limited", data: 15 },
      default_deck_copy_limit: { type: "UpTo", data: 4 },
    },
    legality: {
      legal_sets: null,
      banned: [],
      restricted: [],
      legacy: {
        mana_burn: "Modern",
        damage_timing: "Modern",
        wish_scope: "PostM10SideboardOnly",
        legend_rule_scope: "Modern",
      },
    },
  };
}

function customConfig(id = 0): FormatConfig {
  return {
    format: `Custom:${id}`,
    starting_life: 20,
    min_players: 2,
    max_players: 4,
    deck_size: { type: "Minimum", data: 60 },
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: false,
    uses_commander: false,
    supplies_fixed_deck: false,
    sideboard_policy: { type: "Limited", data: 15 },
    default_deck_copy_limit: { type: "UpTo", data: 4 },
    allow_debug_actions: false,
    custom_rules: customRules(id),
  };
}

describe("isFormatConfigShape", () => {
  it("accepts a well-formed built-in config", () => {
    expect(isFormatConfigShape(builtInConfig())).toBe(true);
  });

  it("accepts a well-formed custom config", () => {
    expect(isFormatConfigShape(customConfig())).toBe(true);
    expect(isFormatConfigShape(customConfig(7))).toBe(true);
  });

  it("rejects non-objects outright", () => {
    for (const value of [null, undefined, 3, "Commander", [], true]) {
      expect(isFormatConfigShape(value)).toBe(false);
    }
  });

  // Every REQUIRED field, one at a time. A guard that silently stops checking a
  // field after a refactor is the failure mode this catches.
  it.each([
    "format",
    "starting_life",
    "min_players",
    "max_players",
    "deck_size",
    "singleton",
    "command_zone",
    // Required (`number | null`, no `?:`) — the engine's `commander_damage_threshold`
    // carries no `#[serde(skip_serializing_if)]`, so a genuine serialized
    // FormatConfig always has the key. A blob missing it entirely is not a
    // real serialization, not a `None`. See the sibling `isOptionalInteger`
    // helper for fields that ARE genuinely allowed to omit their key.
    "commander_damage_threshold",
    "team_based",
    "uses_commander",
    "sideboard_policy",
    "default_deck_copy_limit",
    "allow_debug_actions",
  ])("rejects a config missing %s", (field) => {
    const config: Record<string, unknown> = { ...builtInConfig() };
    delete config[field];
    expect(isFormatConfigShape(config)).toBe(false);
  });

  // The three serde-optional fields. Absent is legal (an older payload simply
  // omits them); present-but-malformed is not. These are the ones easiest to
  // forget precisely because they are invisible in most payloads.
  it("accepts a config omitting the serde-optional fields", () => {
    const {
      range_of_influence: _roi,
      supplies_fixed_deck: _fixed,
      ...withoutOptionals
    } = builtInConfig();
    expect(isFormatConfigShape(withoutOptionals)).toBe(true);
  });

  it("rejects a malformed range_of_influence", () => {
    expect(
      isFormatConfigShape({ ...builtInConfig(), range_of_influence: { default_range: "one" } }),
    ).toBe(false);
    expect(
      isFormatConfigShape({
        ...builtInConfig(),
        range_of_influence: { default_range: 1, player_overrides: { "0": "two" } },
      }),
    ).toBe(false);
  });

  it("rejects a malformed archenemy_player", () => {
    expect(isFormatConfigShape({ ...builtInConfig(), archenemy_player: "0" })).toBe(false);
  });

  it("accepts a well-formed archenemy_player", () => {
    expect(isFormatConfigShape({ ...builtInConfig(), archenemy_player: 0 })).toBe(true);
    expect(isFormatConfigShape({ ...builtInConfig(), archenemy_player: null })).toBe(true);
  });

  it("rejects a malformed supplies_fixed_deck", () => {
    expect(isFormatConfigShape({ ...builtInConfig(), supplies_fixed_deck: "yes" })).toBe(false);
  });

  it("rejects a bare integer deck_size (the pre-DeckSizeRule wire shape)", () => {
    expect(isFormatConfigShape({ ...builtInConfig(), deck_size: 100 })).toBe(false);
  });

  it("rejects a tagged union with the wrong payload type", () => {
    expect(
      isFormatConfigShape({ ...builtInConfig(), sideboard_policy: { type: "Limited" } }),
    ).toBe(false);
    expect(
      isFormatConfigShape({ ...builtInConfig(), default_deck_copy_limit: { type: "UpTo" } }),
    ).toBe(false);
  });

  // The engine's `validate_custom_rules_consistency` biconditional, mirrored:
  // `format == Custom(id) <=> custom_rules == Some(rules with that id)`.
  it("rejects a custom format with no custom_rules", () => {
    const { custom_rules: _dropped, ...withoutRules } = customConfig();
    expect(isFormatConfigShape(withoutRules)).toBe(false);
  });

  it("rejects a custom format whose rules id disagrees with the format string", () => {
    expect(
      isFormatConfigShape({ ...customConfig(7), custom_rules: customRules(0) }),
    ).toBe(false);
  });

  it("rejects a built-in format that carries custom_rules", () => {
    expect(
      isFormatConfigShape({ ...builtInConfig(), custom_rules: customRules(0) }),
    ).toBe(false);
  });

  it("rejects a custom format whose rules are themselves malformed", () => {
    const broken = customRules();
    expect(
      isFormatConfigShape({
        ...customConfig(),
        custom_rules: { ...broken, legality: { ...broken.legality, banned: "none" } },
      }),
    ).toBe(false);
  });
});

describe("isCustomFormatRulesShape", () => {
  it("accepts well-formed rules", () => {
    expect(isCustomFormatRulesShape(customRules())).toBe(true);
  });

  it("accepts an Enabled command zone with its full payload", () => {
    const rules = customRules();
    expect(
      isCustomFormatRulesShape({
        ...rules,
        structural: {
          ...rules.structural,
          command_zone_mode: {
            Enabled: { commander_damage_threshold: 21, eligibility_rule: "Standard" },
          },
        },
      }),
    ).toBe(true);
  });

  it("rejects an Enabled command zone missing commander_damage_threshold", () => {
    // Same field, same reasoning as FormatConfig's own required
    // commander_damage_threshold: no skip_serializing_if, so a missing key
    // means the blob was never a real serialized CommandZoneMode.
    const rules = customRules();
    expect(
      isCustomFormatRulesShape({
        ...rules,
        structural: {
          ...rules.structural,
          command_zone_mode: { Enabled: { eligibility_rule: "Standard" } },
        },
      }),
    ).toBe(false);
  });

  it("rejects an Enabled command zone with an unknown eligibility rule", () => {
    const rules = customRules();
    expect(
      isCustomFormatRulesShape({
        ...rules,
        structural: {
          ...rules.structural,
          command_zone_mode: {
            Enabled: { commander_damage_threshold: 21, eligibility_rule: "Bogus" },
          },
        },
      }),
    ).toBe(false);
  });

  it("distinguishes a null legal_sets from a missing one", () => {
    const rules = customRules();
    // `null` means unrestricted — legal, and NOT the same claim as [].
    expect(isCustomFormatRulesShape(rules)).toBe(true);
    expect(
      isCustomFormatRulesShape({
        ...rules,
        legality: { ...rules.legality, legal_sets: ["MH3"] },
      }),
    ).toBe(true);
    const { legal_sets: _dropped, ...legalityWithoutSets } = rules.legality;
    expect(
      isCustomFormatRulesShape({ ...rules, legality: legalityWithoutSets }),
    ).toBe(false);
  });

  it("rejects an unimplemented legacy axis value", () => {
    const rules = customRules();
    expect(
      isCustomFormatRulesShape({
        ...rules,
        legality: {
          ...rules.legality,
          legacy: { ...rules.legality.legacy, mana_burn: "Whatever" },
        },
      }),
    ).toBe(false);
  });
});
