import type { GameState, ManaColor, ObjectId, PlayerId, TargetRef } from "../../../adapter/types.ts";
import {
  type DamageCause,
  damageCauseOf,
  type ResolvingOrigin,
  resolvingSourceOf,
} from "../../../animation/damageCause.ts";
import type { AnimationEvent } from "../../../animation/types.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { type AnimationImageSnapshot, visibleAnimationImageSnapshot } from "../ResolvedAnimationImage.tsx";
import {
  type CardFlightSpec,
  type CardFlightSpecContext,
  cardFlightSpecFor,
  flightPresents,
  type RippleLook,
  rippleLookFor,
  type SweepLook,
  type SweepSpec,
  sweepOf,
} from "./cardFlightSpecs.ts";
import type { CounterChange } from "./tallyEffects.ts";

/** A permanent broken apart where it lies. */
export interface CardShatterSpec {
  kind: "shatter";
  objectId: ObjectId;
  /** The face the viewer saw before the event; `null` shows the card back. */
  face: AnimationImageSnapshot | null;
  pace: number;
  owningStepMs: number;
  /** As `CardFlightSpec.snapshotSeq`. */
  snapshotSeq: number;
  /** The front that breaks it when it reaches it; `null` breaks it at once. */
  sweep: SweepSpec | null;
}

/** A permanent exiled from the battlefield, dissolving where it lies. */
export interface ExileDissolveSpec {
  kind: "dissolve";
  objectId: ObjectId;
  /** The face the viewer saw before the event; `null` shows the card back. */
  face: AnimationImageSnapshot | null;
  /** The permanent holding the card in exile (linked exile), if any. */
  holderId: ObjectId | null;
  pace: number;
  owningStepMs: number;
  /** As `CardFlightSpec.snapshotSeq`. */
  snapshotSeq: number;
}

/** A card VFX that happens to a permanent where it lies on the board. */
export type BoardEffectSpec = CardShatterSpec | ExileDissolveSpec;

/** Who a damage strike hits: a player at their HUD, or a permanent where it lies. */
export type DamageStrikeTarget =
  | { kind: "player"; playerId: PlayerId }
  | { kind: "permanent"; objectId: ObjectId; face: AnimationImageSnapshot | null };

/** A spell or ability's damage travelling from its source to its target. */
export interface DamageStrikeSpec {
  kind: "damage";
  cause: DamageCause;
  origin: ResolvingOrigin;
  target: DamageStrikeTarget;
  amount: number;
  pace: number;
  owningStepMs: number;
}

/** A creature's blow landing where its slam strikes; the slam itself, and
 *  the struck card's knockback, are the DOM's. */
export interface DamageBlowSpec {
  kind: "blow";
  /** The striking creature; `null` for a flurry of hits from many. */
  sourceId: ObjectId | null;
  target: TargetRef;
  amount: number;
  pace: number;
  /** When the slam started, on the frame clock (`performance.now()`). */
  startMs: number;
  /** When the slam lands, after `startMs`, already paced. */
  impactDelayMs: number;
}

/** A counter's ripple, travelling from the countering spell or ability to the
 *  spell it counters. */
export interface CounterRippleSpec {
  kind: "ripple";
  origin: ResolvingOrigin;
  /** The countered spell or ability's stack entry. */
  targetId: ObjectId;
  look: RippleLook;
  pace: number;
}

/** A player's life total changing other than by damage a strike or blow shows. */
export interface LifeChangeSpec {
  kind: "life";
  playerId: PlayerId;
  /** Life gained (positive) or lost (negative). */
  amount: number;
  pace: number;
}

/** Counters put on a permanent or removed from it. */
export interface CounterChangeSpec {
  kind: "counter";
  objectId: ObjectId;
  counterType: string;
  change: CounterChange;
  count: number;
  pace: number;
}

/** A destruction or sacrifice a replacement sent elsewhere: its earlier zone
 *  change shows the move, and it presents Classic only if that did not. */
export interface CoveredSpec {
  kind: "covered";
  objectId: ObjectId;
}

/** Everything the card VFX layer presents, by `kind`. */
export type CardVfxSpec =
  | CardFlightSpec
  | BoardEffectSpec
  | DamageStrikeSpec
  | DamageBlowSpec
  | CounterRippleSpec
  | LifeChangeSpec
  | CounterChangeSpec
  | CoveredSpec;

/** The pre-event state `damageCauseOf` reads, when a card VFX layer presents
 *  damage causes; `null` when every hit presents Classic. Hit timing reads it
 *  too, so a life total ticks when the strike lands. */
export function damageCauseState(): GameState | null {
  return useAnimationStore.getState().cardVfxReady ? useGameStore.getState().gameState : null;
}

// CR 701.8a / CR 701.21a: a destroyed or sacrificed permanent moves to its
// owner's graveyard, unless a replacement (CR 614.1a) sends it elsewhere. Its
// move to exile then dissolves, and to a hand or library flies.
function coveredSpecFor(event: AnimationEvent, { post, pace }: CardFlightSpecContext): CoveredSpec | null {
  if (pace <= 0 || (event.type !== "CreatureDestroyed" && event.type !== "PermanentSacrificed")) return null;
  const objectId = event.data.object_id;
  const zone = post?.objects[objectId]?.zone;
  const presented = zone === "Exile" || (zone !== undefined && flightPresents("Battlefield", zone));
  return presented ? { kind: "covered", objectId } : null;
}

// A black spell's destruction rolls over the board as smoke, and a white
// one's as light; a spell of both colours is smoke.
function destructionLook(colors: readonly ManaColor[]): SweepLook | null {
  if (colors.includes("Black")) return "smoke";
  return colors.includes("White") ? "light" : null;
}

// CR 701.8a: a destroyed permanent moves from the battlefield to its owner's
// graveyard; the shatter shows it breaking where it lay. A spell destroying
// several at once sweeps a front over them that breaks each in turn.
function cardShatterSpecFor(
  event: AnimationEvent,
  { pre, pace, owningStepMs, snapshotSeq, stepEvents }: CardFlightSpecContext,
): CardShatterSpec | null {
  if (pace <= 0 || event.type !== "CreatureDestroyed") return null;
  const objectId = event.data.object_id;
  const object = pre?.objects[objectId];
  if (!object) return null;
  const destroyed = stepEvents.flatMap((other) => (other.type === "CreatureDestroyed" ? [other.data.object_id] : []));
  return {
    kind: "shatter",
    objectId,
    face: visibleAnimationImageSnapshot(object),
    pace,
    owningStepMs,
    snapshotSeq,
    sweep: sweepOf(destroyed, destructionLook, pre),
  };
}

// CR 701.13a: an exiled object moves to the exile zone; the dissolve shows a
// permanent leaving the battlefield that way. A permanent that exiled it and
// holds it (the engine's linked-exile view) is where its flakes go.
function exileDissolveSpecFor(
  event: AnimationEvent,
  { pre, post, pace, owningStepMs, snapshotSeq }: CardFlightSpecContext,
): ExileDissolveSpec | null {
  if (pace <= 0 || event.type !== "ZoneChanged") return null;
  const { object_id: objectId, from, to } = event.data;
  if (from !== "Battlefield" || to !== "Exile") return null;
  const object = pre?.objects[objectId];
  if (!object) return null;
  const links = Object.entries(post?.derived?.linked_exile_ids ?? {});
  const holder = links.find(([, exiled]) => exiled.includes(objectId))?.[0];
  return {
    kind: "dissolve",
    objectId,
    face: visibleAnimationImageSnapshot(object),
    holderId: holder === undefined ? null : Number(holder),
    pace,
    owningStepMs,
    snapshotSeq,
  };
}

function damageStrikeSpecFor(
  event: AnimationEvent,
  { pre, pace, owningStepMs }: CardFlightSpecContext,
): DamageStrikeSpec | null {
  if (pace <= 0 || event.type !== "DamageDealt") return null;
  const cause = damageCauseOf(event, pre);
  if (!cause) return null;
  const { target: ref, amount } = event.data;
  let target: DamageStrikeTarget;
  if ("Player" in ref) {
    target = { kind: "player", playerId: ref.Player };
  } else {
    const object = pre?.objects[ref.Object];
    if (!object) return null;
    target = { kind: "permanent", objectId: ref.Object, face: visibleAnimationImageSnapshot(object) };
  }
  return { kind: "damage", ...cause, target, amount, pace, owningStepMs };
}

// CR 701.6a: to counter a spell or ability is to cancel it, removing it from
// the stack. The ripple leaves from what counters it, as its damage would.
function counterRippleSpecFor(event: AnimationEvent, { pre, pace }: CardFlightSpecContext): CounterRippleSpec | null {
  if (pace <= 0 || event.type !== "SpellCountered") return null;
  const resolving = resolvingSourceOf(event.data.countered_by, pre);
  if (!resolving) return null;
  const { source, origin } = resolving;
  return { kind: "ripple", origin, targetId: event.data.object_id, look: rippleLookFor(source.color), pace };
}

// CR 122.1: a counter is a marker placed on an object; it plays where the
// permanent it is put on or removed from lies.
function counterChangeSpecFor(event: AnimationEvent, { pace }: CardFlightSpecContext): CounterChangeSpec | null {
  if (pace <= 0) return null;
  switch (event.type) {
    case "CounterAdded":
    case "CounterRemoved": {
      const { object_id: objectId, counter_type: counterType, count } = event.data;
      const change = event.type === "CounterAdded" ? "added" : "removed";
      return { kind: "counter", objectId, counterType, change, count, pace };
    }
    default:
      return null;
  }
}

/** The card VFX presentation of `event`, or `null` when it has none (it then
 *  presents Classic). */
export function cardVfxSpecFor(event: AnimationEvent, context: CardFlightSpecContext): CardVfxSpec | null {
  return (
    cardFlightSpecFor(event, context) ??
    coveredSpecFor(event, context) ??
    cardShatterSpecFor(event, context) ??
    exileDissolveSpecFor(event, context) ??
    damageStrikeSpecFor(event, context) ??
    counterRippleSpecFor(event, context) ??
    counterChangeSpecFor(event, context)
  );
}
