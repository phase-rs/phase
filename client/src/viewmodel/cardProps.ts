import type { GameObject, ManaColor, ObjectId } from "../adapter/types";

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
  attachedTo: ObjectId | null;
  attachmentIds: ObjectId[];
  keywords: string[];
  colorIdentity: ManaColor[];
}

export type PTColor = "white" | "green" | "red";

export interface PTDisplay {
  power: number;
  toughness: number;
  powerColor: PTColor;
  toughnessColor: PTColor;
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
    name: obj.name,
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
    counters: Object.entries(obj.counters).map(([type, count]) => ({ type, count })),
    isCreature: obj.card_types.core_types.includes("Creature"),
    isLand: obj.card_types.core_types.includes("Land"),
    attachedTo: obj.attached_to,
    attachmentIds: obj.attachments,
    keywords: obj.keywords,
    colorIdentity: obj.color,
  };
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
    toughness: obj.toughness,
    powerColor,
    toughnessColor,
  };
}
