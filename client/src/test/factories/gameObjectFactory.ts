import { Factory } from "fishery";

import type { CardType, GameObject, ObjectId, PlayerId } from "../../adapter/types.ts";

const defaultCardType: CardType = {
  supertypes: [],
  core_types: ["Artifact"],
  subtypes: [],
};

const mergeCardType = (overrides: Partial<CardType> = {}): CardType => ({
  ...defaultCardType,
  ...overrides,
  supertypes: [...(overrides.supertypes ?? defaultCardType.supertypes)],
  core_types: [...(overrides.core_types ?? defaultCardType.core_types)],
  subtypes: [...(overrides.subtypes ?? defaultCardType.subtypes)],
});

/**
 * Convenience-method factory for `GameObject`. Methods chain and compose:
 * `gameObjectFactory.creature(3, 3).legendary().inHand().named("Sigarda").build()`.
 * Prefer chaining these over passing raw override objects to `.build()`.
 */
export class GameObjectFactory extends Factory<GameObject> {
  // --- Card types (params deep-merge, so `.creature().legendary()` composes) ---
  creature(power = 2, toughness = 2) {
    return this.params({
      card_types: { core_types: ["Creature"] },
      power,
      toughness,
      base_power: power,
      base_toughness: toughness,
    });
  }

  artifact() {
    return this.params({ card_types: { core_types: ["Artifact"] } });
  }

  enchantment() {
    return this.params({ card_types: { core_types: ["Enchantment"] } });
  }

  land() {
    return this.params({ card_types: { core_types: ["Land"] } });
  }

  sorcery() {
    return this.params({ card_types: { core_types: ["Sorcery"] } });
  }

  instant() {
    return this.params({ card_types: { core_types: ["Instant"] } });
  }

  planeswalker(loyalty = 3) {
    return this.params({ card_types: { core_types: ["Planeswalker"] }, loyalty });
  }

  legendary() {
    return this.params({ card_types: { supertypes: ["Legendary"] } });
  }

  // --- Zones ---
  inHand() {
    return this.params({ zone: "Hand", entered_battlefield_turn: null });
  }

  onBattlefield() {
    return this.params({ zone: "Battlefield" });
  }

  inGraveyard() {
    return this.params({ zone: "Graveyard", entered_battlefield_turn: null });
  }

  inExile() {
    return this.params({ zone: "Exile", entered_battlefield_turn: null });
  }

  inCommandZone() {
    return this.params({ zone: "Command", entered_battlefield_turn: null });
  }

  // --- Object state ---
  tapped() {
    return this.params({ tapped: true });
  }

  named(name: string) {
    return this.params({ name });
  }

  withId(id: ObjectId) {
    return this.params({ id, card_id: id });
  }

  ownedBy(player: PlayerId) {
    return this.params({ owner: player, controller: player });
  }

  controlledBy(player: PlayerId) {
    return this.params({ controller: player });
  }

  withCost(shards: string[], generic = 0) {
    return this.params({ mana_cost: { type: "Cost", shards, generic } });
  }

  commander() {
    return this.inCommandZone().legendary().creature(3, 3).params({
      is_commander: true,
      commander_tax: 0,
    });
  }
}

export const gameObjectFactory = GameObjectFactory.define(({ sequence }): GameObject => ({
  id: sequence,
  card_id: sequence,
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
  name: "Mock Object",
  power: null,
  toughness: null,
  loyalty: null,
  card_types: mergeCardType(),
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
  entered_battlefield_turn: 1,
}));

export const buildGameObject = (overrides: Partial<GameObject> = {}): GameObject => {
  const { card_types, ...otherOverrides } = overrides;

  return {
    ...gameObjectFactory.build(),
    ...otherOverrides,
    ...(card_types ? { card_types: mergeCardType(card_types) } : {}),
  };
};

export const buildGameObjectWithCoreTypes = (
  coreTypes: string[],
  overrides: Partial<GameObject> = {},
): GameObject => {
  return buildGameObject({
    ...overrides,
    card_types: mergeCardType({
      ...(overrides.card_types ?? {}),
      core_types: coreTypes,
    }),
  });
};

export const buildObjectMap = (...objects: GameObject[]): Record<string, GameObject> => {
  return Object.fromEntries(objects.map((object) => [String(object.id), object]));
};

export const buildCommanderGameObject = (
  overrides: Partial<GameObject> = {},
): GameObject => {
  return buildGameObject({
    id: 101,
    card_id: 201,
    owner: 0,
    controller: 0,
    zone: "Command",
    name: "Mock Commander",
    power: 3,
    toughness: 3,
    card_types: {
      supertypes: ["Legendary"],
      core_types: ["Creature"],
      subtypes: [],
    },
    mana_cost: { type: "Cost", shards: ["Green"], generic: 2 },
    color: ["Green"],
    base_power: 3,
    base_toughness: 3,
    base_color: ["Green"],
    entered_battlefield_turn: null,
    is_commander: true,
    commander_tax: 0,
    ...overrides,
  });
};
