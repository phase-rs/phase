import type {
  AttackerInfo,
  CombatState,
  GameObject,
  ObjectCounterDisplay,
  ObjectId,
  PlayerId,
} from "../adapter/types";
import { publicName, toCardProps } from "./cardProps";
import type { CardViewProps } from "./cardProps";

function canGroup(obj: GameObject, ringBearerIds: ReadonlySet<ObjectId>): boolean {
  // Ring-bearers (CR 701.54) must never be hidden behind a same-named
  // non-bearer representative in a collapsed/stacked group display — render
  // them solo so the ring-bearer badge in PermanentCard is always visible.
  return obj.attachments.length === 0 && !ringBearerIds.has(obj.id);
}

function groupKey(
  obj: GameObject,
  counterDisplay: Record<string, ObjectCounterDisplay> | undefined,
): string {
  const kw = obj.keywords.map((k) => typeof k === "string" ? k : JSON.stringify(k)).sort().join(",");
  const colors = [...obj.color].sort().join("");
  // CR 122.1 + CR 732.2a: the group identity for counters is the engine's RENDERED rows, not the
  // raw map — two permanents that render different pills must not collapse into one representative,
  // and an `∞`-marked member must never hide behind an unmarked one. The engine already orders the
  // rows deterministically (`∞` first, then CounterType order), so this is a straight join, never a
  // sort. Zero-count entries are absent from the engine's rows (CR 122.1), so two permanents
  // differing only by a `{charge: 0}` entry now group TOGETHER; they render identically, which is
  // what this key is for.
  const display = counterDisplay?.[String(obj.id)];
  const counters = [
    ...(display?.pills ?? []),
    ...(display?.loyalty ? [display.loyalty] : []),
  ]
    .map((r) => `${r.counter}:${r.count}:${r.magnitude ?? "Finite"}`)
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
  /**
   * CR 732.2a: every member of this group is part of an accepted object-growth
   * loop's engine-authored "∞ pile", so the group renders `∞` instead of `×N`.
   */
  isUnboundedPile: boolean;
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
const NO_UNBOUNDED_PILE: ReadonlySet<ObjectId> = new Set();

/**
 * `counterDisplay` is POSITIONALLY REQUIRED even though it accepts `undefined`: `ringBearerIds`
 * and `unboundedPileIds` are *enrichment* inputs whose omission only degrades a badge, while
 * `counterDisplay` is a *correctness* input to the group identity itself — a default would
 * silently produce wrong grouping at any site that forgot it. Defaults are for enrichment;
 * correctness inputs get the compiler.
 */
export function groupByName(
  objects: GameObject[],
  ringBearerIds: ReadonlySet<ObjectId> = NO_RING_BEARERS,
  unboundedPileIds: ReadonlySet<ObjectId> = NO_UNBOUNDED_PILE,
  counterDisplay: Record<string, ObjectCounterDisplay> | undefined,
): GroupedPermanent[] {
  const groups = new Map<string, GameObject[]>();

  for (const obj of objects) {
    if (!canGroup(obj, ringBearerIds)) {
      // Ungroupable objects (attachments, ring-bearers) get their own entry
      groups.set(`__solo_${obj.id}`, [obj]);
      continue;
    }

    const key = groupKey(obj, counterDisplay);
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
      // `.every()` is the fail-safe direction: groupKey (above) already splits on
      // tapped/power/toughness/counters/damage/summoning-sickness, so those never make
      // a group heterogeneous in pile membership. Fields object_content_eq compares but
      // groupKey omits could split membership within a visual group — in which case
      // `.every()` correctly degrades to `×N` (never a false `∞`).
      isUnboundedPile: members.every((m) => unboundedPileIds.has(m.id)),
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
