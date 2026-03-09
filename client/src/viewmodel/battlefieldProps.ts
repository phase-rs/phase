import type { GameObject, ObjectId } from "../adapter/types";
import { toCardProps } from "./cardProps";
import type { CardViewProps } from "./cardProps";

export interface BattlefieldPartition {
  creatures: ObjectId[];
  lands: ObjectId[];
  other: ObjectId[];
}

export interface GroupedPermanent {
  name: string;
  ids: ObjectId[];
  count: number;
  representative: CardViewProps;
}

export function partitionByType(objects: GameObject[]): BattlefieldPartition {
  const creatures: ObjectId[] = [];
  const lands: ObjectId[] = [];
  const other: ObjectId[] = [];

  for (const obj of objects) {
    if (obj.card_types.core_types.includes("Land")) {
      lands.push(obj.id);
    } else if (obj.card_types.core_types.includes("Creature")) {
      creatures.push(obj.id);
    } else {
      other.push(obj.id);
    }
  }

  return { creatures, lands, other };
}

function canGroup(obj: GameObject): boolean {
  return obj.attachments.length === 0 && Object.keys(obj.counters).length === 0;
}

function groupKey(obj: GameObject): string {
  return `${obj.name}::${obj.tapped}`;
}

export function groupByName(objects: GameObject[]): GroupedPermanent[] {
  const groups = new Map<string, GameObject[]>();

  for (const obj of objects) {
    if (!canGroup(obj)) {
      // Ungroupable objects get their own entry with a unique key
      groups.set(`__solo_${obj.id}`, [obj]);
      continue;
    }

    const key = groupKey(obj);
    const existing = groups.get(key);
    if (existing) {
      existing.push(obj);
    } else {
      groups.set(key, [obj]);
    }
  }

  const result: GroupedPermanent[] = [];

  for (const members of groups.values()) {
    result.push({
      name: members[0].name,
      ids: members.map((m) => m.id),
      count: members.length,
      representative: toCardProps(members[0]),
    });
  }

  return result;
}
