import type { AttackerInfo, CombatState, GameObject, ObjectId, PlayerId } from "../adapter/types";
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

export function groupByName(objects: GameObject[]): GroupedPermanent[] {
  return objects.map((obj) => ({
    name: obj.name,
    ids: [obj.id],
    count: 1,
    representative: toCardProps(obj),
  }));
}

/** Group attackers by their defending player target. */
export function groupAttackersByTarget(
  combat: CombatState | null,
): Map<PlayerId, AttackerInfo[]> {
  const groups = new Map<PlayerId, AttackerInfo[]>();
  if (!combat) return groups;

  for (const attacker of combat.attackers) {
    const group = groups.get(attacker.defending_player);
    if (group) {
      group.push(attacker);
    } else {
      groups.set(attacker.defending_player, [attacker]);
    }
  }

  return groups;
}

/** Get attacker IDs targeting a specific defending player. */
export function getAttackersTargeting(
  combat: CombatState | null,
  defendingPlayer: PlayerId,
): ObjectId[] {
  if (!combat) return [];
  return combat.attackers
    .filter((a) => a.defending_player === defendingPlayer)
    .map((a) => a.object_id);
}

/** Check if an attacker is targeting the given defending player. */
export function isAttackerTargetingPlayer(
  combat: CombatState | null,
  attackerId: ObjectId,
  defendingPlayer: PlayerId,
): boolean {
  if (!combat) return false;
  return combat.attackers.some(
    (a) => a.object_id === attackerId && a.defending_player === defendingPlayer,
  );
}
