import type { AttackerInfo, CombatState, GameObject, ObjectId, PlayerId } from "../adapter/types";
import { publicName, toCardProps } from "./cardProps";
import type { CardViewProps } from "./cardProps";

function canGroup(obj: GameObject): boolean {
  return obj.attachments.length === 0;
}

function groupKey(obj: GameObject): string {
  const kw = obj.keywords.map((k) => typeof k === "string" ? k : JSON.stringify(k)).sort().join(",");
  const colors = [...obj.color].sort().join("");
  return `${publicName(obj)}|${obj.tapped}|${obj.face_down}|${obj.flipped}|${obj.transformed}|${obj.power}|${obj.toughness}|${obj.loyalty}|${obj.damage_marked}|${obj.has_summoning_sickness}|${obj.class_level ?? ""}|${colors}|${kw}|${JSON.stringify(obj.counters)}`;
}

export interface BattlefieldPartition {
  creatures: ObjectId[];
  lands: ObjectId[];
  support: ObjectId[];
  planeswalkers: ObjectId[];
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
  const support: ObjectId[] = [];
  const planeswalkers: ObjectId[] = [];
  const other: ObjectId[] = [];

  for (const obj of objects) {
    // Attached permanents render through their host's surface, not the main
    // battlefield rows: Object-host attachments peek behind the host card
    // (see attachments.map in PermanentCard); Player-host attachments
    // (Curse cycle, Faith's Fetters-class) render alongside the player's
    // HUD plate (see PlayerAttachedAuras). Either way, double-rendering them
    // here would put two copies on screen.
    if (obj.attached_to !== null) continue;
    const coreTypes = obj.card_types.core_types;

    if (coreTypes.includes("Creature")) {
      creatures.push(obj.id);
    } else if (coreTypes.includes("Land")) {
      lands.push(obj.id);
    } else if (coreTypes.includes("Planeswalker")) {
      planeswalkers.push(obj.id);
    } else if (
      coreTypes.includes("Artifact")
      || coreTypes.includes("Enchantment")
      || obj.card_id === 0
    ) {
      support.push(obj.id);
    } else {
      other.push(obj.id);
    }
  }

  return { creatures, lands, support, planeswalkers, other };
}

export function groupByName(objects: GameObject[]): GroupedPermanent[] {
  const groups = new Map<string, GameObject[]>();

  for (const obj of objects) {
    if (!canGroup(obj)) {
      // Ungroupable objects (attachments, counters) get their own entry
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
      name: publicName(members[0]),
      ids: members.map((m) => m.id),
      count: members.length,
      representative: toCardProps(members[0]),
    });
  }

  return result;
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

/** Get attacker IDs directly targeting a specific defending player (not their planeswalkers/battles). */
export function getAttackersTargeting(
  combat: CombatState | null,
  defendingPlayer: PlayerId,
): ObjectId[] {
  if (!combat) return [];
  return combat.attackers
    .filter((a) => a.attack_target.type === "Player" && a.attack_target.data === defendingPlayer)
    .map((a) => a.object_id);
}

/** Check if an attacker is directly targeting the given defending player (not their planeswalkers/battles). */
export function isAttackerTargetingPlayer(
  combat: CombatState | null,
  attackerId: ObjectId,
  defendingPlayer: PlayerId,
): boolean {
  if (!combat) return false;
  return combat.attackers.some(
    (a) => a.object_id === attackerId
      && a.attack_target.type === "Player"
      && a.attack_target.data === defendingPlayer,
  );
}
