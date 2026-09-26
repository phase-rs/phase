import type {
  GameObject,
  Keyword,
  ManaColor,
  ManaCost,
  ObjectAttribution,
  ObjectId,
} from "../../adapter/types.ts";
import {
  computePTDisplay,
  formatTypeLine,
  publicName,
  type PTColor,
} from "../../viewmodel/cardProps.ts";
import { manaCostToShards } from "../../viewmodel/costLabel.ts";
import { getKeywordDisplayText, sortKeywords } from "../../viewmodel/keywordProps.ts";

export interface TabletopCounterPresentation {
  count: number;
  type: string;
}

/**
 * Renderer-neutral card data consumed by both the DOM hand and Three.js card
 * textures. Every characteristic is copied from an engine-authored field; this
 * projection only formats those values for display.
 */
export interface TabletopCardPresentation {
  objectId: ObjectId;
  faceDown: boolean;
  name: string;
  manaSymbols: string[];
  manaCostReduced: boolean;
  typeLine: string;
  rulesText: string | null;
  keywords: string[];
  power: number | null;
  toughness: number | null;
  powerColor: PTColor;
  toughnessColor: PTColor;
  damageMarked: number;
  loyalty: number | null;
  counters: TabletopCounterPresentation[];
  colors: ManaColor[];
  modifierCount: number;
}

export function countAttributedModifiers(attribution: ObjectAttribution | undefined): number {
  if (!attribution?.by_layer) return 0;
  return Object.values(attribution.by_layer).reduce(
    (count, refs) => count + (refs?.length ?? 0),
    0,
  );
}

export function buildTabletopCardPresentation(
  object: GameObject,
  displayCost: ManaCost = object.mana_cost,
  attribution?: ObjectAttribution,
  rulesText: string | null = null,
  manaCostReduced = false,
): TabletopCardPresentation {
  const manaSymbols = manaCostToShards(displayCost);
  const ptDisplay = computePTDisplay(object);
  return {
    objectId: object.id,
    faceDown: object.face_down,
    name: publicName(object),
    manaSymbols:
      manaCostReduced && manaSymbols.length === 0 ? ["0"] : manaSymbols,
    manaCostReduced,
    typeLine: formatTypeLine(object.card_types, object.keywords),
    rulesText,
    keywords: formatKeywords(object.keywords),
    power: object.power,
    toughness: object.toughness,
    powerColor: ptDisplay?.powerColor ?? "white",
    toughnessColor: ptDisplay?.toughnessColor ?? "white",
    damageMarked: object.damage_marked,
    loyalty: object.loyalty,
    counters: Object.entries(object.counters)
      .filter((entry): entry is [string, number] => entry[1] != null && entry[1] !== 0)
      .map(([type, count]) => ({ type, count })),
    colors: [...object.color],
    modifierCount: countAttributedModifiers(attribution),
  };
}

function formatKeywords(keywords: Keyword[]): string[] {
  return sortKeywords(keywords).map(getKeywordDisplayText);
}

export function tabletopCardRevision(presentation: TabletopCardPresentation): string {
  return JSON.stringify(presentation);
}
