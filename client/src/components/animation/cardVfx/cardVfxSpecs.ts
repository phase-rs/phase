import type { ObjectId, PlayerId } from "../../../adapter/types.ts";
import type { AnimationEvent } from "../../../animation/types.ts";
import { type AnimationImageSnapshot, visibleAnimationImageSnapshot } from "../ResolvedAnimationImage.tsx";
import { type CardFlightSpec, type CardFlightSpecContext, cardFlightSpecFor } from "./cardFlightSpecs.ts";

/** A permanent broken apart where it lies. */
export interface CardShatterSpec {
  kind: "shatter";
  objectId: ObjectId;
  ownerId: PlayerId;
  /** The face the viewer saw before the event; `null` shows the card back. */
  face: AnimationImageSnapshot | null;
  pace: number;
  owningStepMs: number;
}

/** A permanent exiled from the battlefield, dissolving where it lies. */
export interface ExileDissolveSpec {
  kind: "dissolve";
  objectId: ObjectId;
  ownerId: PlayerId;
  /** The face the viewer saw before the event; `null` shows the card back. */
  face: AnimationImageSnapshot | null;
  /** The permanent holding the card in exile (linked exile), if any. */
  holderId: ObjectId | null;
  pace: number;
  owningStepMs: number;
}

/** A card VFX that happens to a permanent where it lies on the board. */
export type BoardEffectSpec = CardShatterSpec | ExileDissolveSpec;

/** Everything the card VFX layer presents, by `kind`. */
export type CardVfxSpec = CardFlightSpec | BoardEffectSpec;

// CR 701.8a: a destroyed permanent moves from the battlefield to its owner's
// graveyard; the shatter shows it breaking where it lay.
function cardShatterSpecFor(
  event: AnimationEvent,
  { pre, pace, owningStepMs }: CardFlightSpecContext,
): CardShatterSpec | null {
  if (pace <= 0 || event.type !== "CreatureDestroyed") return null;
  const objectId = event.data.object_id;
  const object = pre?.objects[objectId];
  return object
    ? { kind: "shatter", objectId, ownerId: object.owner, face: visibleAnimationImageSnapshot(object), pace, owningStepMs }
    : null;
}

// CR 701.13a: an exiled object moves to the exile zone; the dissolve shows a
// permanent leaving the battlefield that way. A permanent that exiled it and
// holds it (the engine's linked-exile view) is where its flakes go.
function exileDissolveSpecFor(
  event: AnimationEvent,
  { pre, post, pace, owningStepMs }: CardFlightSpecContext,
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
    ownerId: object.owner,
    face: visibleAnimationImageSnapshot(object),
    holderId: holder === undefined ? null : Number(holder),
    pace,
    owningStepMs,
  };
}

/** The card VFX presentation of `event`, or `null` when it has none (it then
 *  presents Classic). */
export function cardVfxSpecFor(event: AnimationEvent, context: CardFlightSpecContext): CardVfxSpec | null {
  return (
    cardFlightSpecFor(event, context) ??
    cardShatterSpecFor(event, context) ??
    exileDissolveSpecFor(event, context)
  );
}
