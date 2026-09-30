import type { GameState, ManaColor, ObjectId, PlayerId } from "../adapter/types";
import type { AnimationEvent } from "./types";

/** How a spell or ability's damage looks on its way to its target. */
export type DamageCause = "fire" | "lightning";

/** Where a damage cause leaves from: the source permanent, or the resolving
 *  stack entry when the source is on the stack or has left the battlefield. */
export interface DamageCauseOrigin {
  zone: "Stack" | "Battlefield";
  objectId: ObjectId;
  ownerId: PlayerId;
}

export interface DamageCauseOf {
  cause: DamageCause;
  origin: DamageCauseOrigin;
}

// Blue and colourless sources strike with lightning; every other colour throws fire.
function causeFor(colors: readonly ManaColor[]): DamageCause {
  return colors.length === 0 || colors.includes("Blue") ? "lightning" : "fire";
}

/** CR 120.2b: damage dealt as the effect of a spell or ability comes from the
 *  object it names. Damage the resolving spell or ability deals from itself or
 *  its own source travels to its target as a cause. Other damage has none:
 *  combat damage, a mana ability's, or a creature a spell makes deal damage. */
export function damageCauseOf(event: AnimationEvent, pre: GameState | null): DamageCauseOf | null {
  if (event.type !== "DamageDealt" || event.data.is_combat || !pre) return null;
  const sourceId = event.data.source_id;
  const resolving = pre.stack[pre.stack.length - 1];
  if (!resolving || (resolving.id !== sourceId && resolving.source_id !== sourceId)) return null;
  const source = pre.objects[sourceId];
  if (!source) return null;
  // CR 113.7a: an ability whose source has left the battlefield still deals
  // its damage; it leaves from the ability's stack entry.
  const origin: DamageCauseOrigin =
    source.zone === "Battlefield"
      ? { zone: "Battlefield", objectId: sourceId, ownerId: source.owner }
      : { zone: "Stack", objectId: resolving.id, ownerId: source.owner };
  return { cause: causeFor(source.color), origin };
}
