import type { AttackerInfo, CombatState, GameObject, ObjectId, PlayerId } from "../adapter/types";
import { publicName, toCardProps } from "./cardProps";
import type { CardViewProps } from "./cardProps";

function canGroup(obj: GameObject, ringBearerIds: ReadonlySet<ObjectId>): boolean {
  // Ring-bearers (CR 701.54) must never be hidden behind a same-named
  // non-bearer representative in a collapsed/stacked group display — render
  // them solo so the ring-bearer badge in PermanentCard is always visible.
  return obj.attachments.length === 0 && !ringBearerIds.has(obj.id);
}

function groupKey(obj: GameObject): string {
  const kw = obj.keywords.map((k) => typeof k === "string" ? k : JSON.stringify(k)).sort().join(",");
  const colors = [...obj.color].sort().join("");
  // counters is a known-shape Partial<Record<CounterType, number>>. Build the
  // key from sorted entries rather than JSON.stringify — cheaper (no serialize
  // allocation per permanent on every board rebuild) and order-independent, so
  // two identical permanents always land in the same group regardless of the
  // order their counters were applied (the old stringify could split them by
  // insertion order; this matches the sorted keyword key above).
  const counters = Object.entries(obj.counters ?? {})
    .sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0))
    .map(([type, n]) => `${type}:${n}`)
    .join(",");
  // Tokens that share a display name (e.g. SOS vs BLC Pest) differ by rules text
  // and/or preset art — include both so visually distinct tokens never stack.
  const tokenRules = obj.token_rules_text ?? "";
  const tokenPreset = obj.token_image_ref?.preset_id ?? "";
  const isToken = obj.is_token ?? false;
  const isCommander = obj.is_commander ?? false;
  return `${publicName(obj)}|${obj.tapped}|${obj.face_down}|${obj.flipped}|${obj.transformed}|${obj.power}|${obj.toughness}|${obj.loyalty}|${obj.damage_marked}|${obj.has_summoning_sickness}|${obj.class_level ?? ""}|${colors}|${kw}|${counters}|${tokenRules}|${tokenPreset}|${isToken}|${isCommander}`;
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
    const subtypes = obj.card_types.subtypes;
    const isAttachmentKind =
      subtypes.includes("Aura")
      || subtypes.includes("Equipment")
      || subtypes.includes("Fortification");
    // True attachment kinds render through their host surface instead of the
    // main battlefield rows. Do not hide arbitrary permanents just because the
    // engine gives them an attached_to relationship.
    if (obj.attached_to !== null && isAttachmentKind) continue;
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

const NO_RING_BEARERS: ReadonlySet<ObjectId> = new Set();

export function groupByName(
  objects: GameObject[],
  ringBearerIds: ReadonlySet<ObjectId> = NO_RING_BEARERS,
): GroupedPermanent[] {
  const groups = new Map<string, GameObject[]>();

  for (const obj of objects) {
    if (!canGroup(obj, ringBearerIds)) {
      // Ungroupable objects (attachments, ring-bearers) get their own entry
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
