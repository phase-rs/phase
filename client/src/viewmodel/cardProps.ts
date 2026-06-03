import type { AttachTarget, CardType, GameObject, Keyword, ManaColor, ObjectId } from "../adapter/types";
import { isChangeling } from "./keywordProps";

const ROMAN = ["", "I", "II", "III", "IV", "V"] as const;
export const FACE_DOWN_CARD_NAME = "Face-down card";
/** Convert a small integer (1–5) to a Roman numeral string. */
export function toRoman(n: number): string { return ROMAN[n] ?? String(n); }

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

export function formatCounterTooltip(type: string, count: number): string {
  const label = formatCounterType(type);
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
