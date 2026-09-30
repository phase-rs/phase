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

/** Everything the card VFX layer presents, by `kind`. */
export type CardVfxSpec = CardFlightSpec | CardShatterSpec;

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

/** The card VFX presentation of `event`, or `null` when it has none (it then
 *  presents Classic). */
export function cardVfxSpecFor(event: AnimationEvent, context: CardFlightSpecContext): CardVfxSpec | null {
  return cardFlightSpecFor(event, context) ?? cardShatterSpecFor(event, context);
}
