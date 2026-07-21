import type { AttachTarget, CardType, GameObject, Keyword, ManaColor, ObjectId } from "../adapter/types";
import { isChangeling } from "./keywordProps";

const ROMAN = ["", "I", "II", "III", "IV", "V"] as const;
export const FACE_DOWN_CARD_NAME = "Face-down card";
/** Convert a small integer (1–5) to a Roman numeral string. Values outside the
 *  table fall back to the arabic numeral for a positive integer, or "" for a
 *  non-positive / non-integer input — so a missing/NaN class level renders blank
 *  rather than the literal string "undefined"/"NaN" (`ROMAN[undefined]` is
 *  `undefined`, and the old `?? String(n)` stringified it onto the card badge). */
export function toRoman(n: number): string {
  return ROMAN[n] ?? (Number.isInteger(n) && n > 0 ? String(n) : "");
}

export interface CardViewProps {
  id: ObjectId;
  name: string;
  tapped: boolean;
  power: number | null;
  toughness: number | null;
  basePower: number | null;
  baseToughness: number | null;
  damageMarked: number;
  effectiveToughness: number | null;
  isPowerBuffed: boolean;
  isPowerDebuffed: boolean;
  isToughnessBuffed: boolean;
  isToughnessDebuffed: boolean;
  counters: Array<{ type: string; count: number }>;
  isCreature: boolean;
  isLand: boolean;
  attachedTo: AttachTarget | null;
  attachmentIds: ObjectId[];
  keywords: Keyword[];
  colorIdentity: ManaColor[];
}

export type PTColor = "white" | "green" | "red";

export interface PTDisplay {
  power: number;
  toughness: number;
  powerColor: PTColor;
  toughnessColor: PTColor;
}

export function publicName(obj: GameObject): string {
  return obj.face_down ? FACE_DOWN_CARD_NAME : obj.name;
}

export function toCardProps(obj: GameObject): CardViewProps {
  const isPowerBuffed = obj.power != null && obj.base_power != null && obj.power > obj.base_power;
  const isPowerDebuffed =
    obj.power != null && obj.base_power != null && obj.power < obj.base_power;
  const isToughnessBuffed =
    obj.toughness != null && obj.base_toughness != null && obj.toughness > obj.base_toughness;
  const isToughnessDebuffed =
    (obj.toughness != null &&
      obj.base_toughness != null &&
      obj.toughness < obj.base_toughness) ||
    obj.damage_marked > 0;

  return {
    id: obj.id,
    name: publicName(obj),
    tapped: obj.tapped,
    power: obj.power,
    toughness: obj.toughness,
    basePower: obj.base_power,
    baseToughness: obj.base_toughness,
    damageMarked: obj.damage_marked,
    effectiveToughness: obj.toughness != null ? obj.toughness - obj.damage_marked : null,
    isPowerBuffed,
    isPowerDebuffed,
    isToughnessBuffed,
    isToughnessDebuffed,
    counters: Object.entries(obj.counters)
      .filter((entry): entry is [string, number] => entry[1] != null)
      .map(([type, count]) => ({ type, count })),
    isCreature: obj.card_types.core_types.includes("Creature"),
    isLand: obj.card_types.core_types.includes("Land"),
    attachedTo: obj.attached_to,
    attachmentIds: obj.attachments,
    keywords: obj.keywords,
    colorIdentity: obj.color,
  };
}

export const COUNTER_COLORS: Record<string, string> = {
  P1P1: "bg-green-600",
  M1M1: "bg-red-600",
  loyalty: "bg-amber-600",
};

export function formatCounterType(type: string): string {
  if (type === "P1P1") return "+1/+1";
  if (type === "M1M1") return "-1/-1";
  return type;
}

/**
 * Counter serde key (from `CounterType::as_str`, e.g. "P1P1", "stun", "charge")
 * → mana-font `ms-counter-*` glyph class. Every value is verified present in
 * `mana-font/css/mana.css` by the guardrail test; counter types with no shipped
 * glyph are absent and render text-only.
 */
export const COUNTER_ICON_CLASS: Record<string, string> = {
  P1P1: "ms-counter-plus",
  M1M1: "ms-counter-minus",
  loyalty: "ms-counter-loyalty",
  lore: "ms-counter-lore",
  stun: "ms-counter-stun",
  shield: "ms-counter-shield",
  time: "ms-counter-time",
  charge: "ms-counter-charge",
  gold: "ms-counter-gold",
  ki: "ms-counter-ki",
  rad: "ms-counter-rad",
  verse: "ms-counter-verse",
  void: "ms-counter-void",
  flame: "ms-counter-flame",
  flood: "ms-counter-flood",
  fungus: "ms-counter-fungus",
  muster: "ms-counter-muster",
  doom: "ms-counter-doom",
  echo: "ms-counter-echo",
  finality: "ms-counter-finality",
  brick: "ms-counter-brick",
  mining: "ms-counter-mining",
  paw: "ms-counter-paw",
  pin: "ms-counter-pin",
  scream: "ms-counter-scream",
  skull: "ms-counter-skull",
  slime: "ms-counter-slime",
  vortex: "ms-counter-vortex",
  goad: "ms-counter-goad",
  damage: "ms-counter-damage",
  deathtouch: "ms-counter-deathtouch",
  devotion: "ms-counter-devotion",
};

/** Resolve a counter type's mana-font glyph class, or null when none ships. */
export function counterIconClass(type: string): string | null {
  return COUNTER_ICON_CLASS[type] ?? null;
}

type CounterTooltipTranslator = (
  key: string,
  options?: { count?: number; label?: string; description?: string },
) => string;

const COUNTER_DESCRIPTION_KEYS: Record<string, string> = {
  P1P1: "counterTooltip.descriptions.p1p1",
  M1M1: "counterTooltip.descriptions.m1m1",
  loyalty: "counterTooltip.descriptions.loyalty",
  lore: "counterTooltip.descriptions.lore",
  stun: "counterTooltip.descriptions.stun",
};

export function formatCounterDescription(
  type: string,
  translate: CounterTooltipTranslator,
): string {
  const label = formatCounterType(type);
  const key = COUNTER_DESCRIPTION_KEYS[type];
  return key
    ? translate(key)
    : translate("counterTooltip.descriptions.generic", { label });
}

export function formatCounterTooltip(
  type: string,
  count: number,
  translate?: CounterTooltipTranslator,
  isUnbounded?: boolean,
): string {
  const label = formatCounterType(type);
  if (translate) {
    // CR 732.2a / CR 701.34a: an unbounded counter renders `∞` on the badge, so its
    // tooltip summary must say "unbounded" rather than interpolate the finite count.
    if (isUnbounded) {
      return translate("counterTooltip.summaryUnbounded", {
        label,
        description: formatCounterDescription(type, translate),
      });
    }
    return translate("counterTooltip.summary", {
      count,
      label,
      description: formatCounterDescription(type, translate),
    });
  }
  if (isUnbounded) {
    return `${label} counters: ∞`;
  }
  return `${label} counter${count !== 1 ? "s" : ""}: ${count}`;
}

export function formatTypeLine(cardTypes: CardType, keywords?: Keyword[]): string {
  const parts: string[] = [];
  if (cardTypes.supertypes.length > 0) parts.push(cardTypes.supertypes.join(" "));
  parts.push(cardTypes.core_types.join(" "));
  const main = parts.join(" ");
  // CR 702.73a: a Changeling object is every creature type. The engine expands
  // its subtypes to the full creature-type list; collapse that to "Changeling"
  // so the type line doesn't overflow the card.
  if (keywords && isChangeling(keywords)) {
    return `${main} \u2014 Changeling`;
  }
  if (cardTypes.subtypes.length > 0) {
    return `${main} \u2014 ${cardTypes.subtypes.join(" ")}`;
  }
  return main;
}

export function computePTDisplay(obj: GameObject): PTDisplay | null {
  if (obj.power == null || obj.toughness == null) return null;

  const powerColor: PTColor =
    obj.base_power != null && obj.power > obj.base_power
      ? "green"
      : obj.base_power != null && obj.power < obj.base_power
        ? "red"
        : "white";

  const toughnessColor: PTColor =
    obj.damage_marked > 0
      ? "red"
      : obj.base_toughness != null && obj.toughness > obj.base_toughness
        ? "green"
        : obj.base_toughness != null && obj.toughness < obj.base_toughness
          ? "red"
          : "white";

  return {
    power: obj.power,
    toughness: obj.damage_marked > 0 ? obj.toughness - obj.damage_marked : obj.toughness,
    powerColor,
    toughnessColor,
  };
}
