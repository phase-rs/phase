import { Factory } from "fishery";

import type { CardType, GameObject } from "../../adapter/types.ts";

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

export const gameObjectFactory = Factory.define<GameObject>(() => ({
  id: 1,
  card_id: 1,
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
