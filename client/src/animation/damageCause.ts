import type { GameObject, GameState, ManaColor, ObjectId, PlayerId, StackEntry } from "../adapter/types";
import type { AnimationEvent } from "./types";

/** How a spell or ability's damage looks on its way to its target. */
export type DamageCause = "fire" | "lightning" | "water";

/** Where a resolving spell or ability's effect leaves from: its source
 *  permanent, or its stack entry when the source is on the stack or has left
 *  the battlefield. */
export interface ResolvingOrigin {
  zone: "Stack" | "Battlefield";
  objectId: ObjectId;
  ownerId: PlayerId;
}

/** The object a resolving spell or ability comes from, and where its effects leave from. */
export interface ResolvingSource {
  source: GameObject;
  origin: ResolvingOrigin;
}

export interface DamageCauseOf {
  cause: DamageCause;
  origin: ResolvingOrigin;
}

/** The entry resolving now: the engine's, when a resolution resumes after a
 *  choice; otherwise the top of the stack, which resolves next (CR 608.1). */
export function resolvingEntry(pre: GameState): StackEntry | undefined {
  return pre.resolving_stack_entry ?? pre.stack[pre.stack.length - 1];
}

/** `sourceId` as the resolving spell, or as the source of the resolving
 *  ability; `null` when it is neither. */
export function resolvingSourceOf(sourceId: ObjectId, pre: GameState | null): ResolvingSource | null {
  const resolving = pre && resolvingEntry(pre);
  if (!resolving || (resolving.id !== sourceId && resolving.source_id !== sourceId)) return null;
  const source = pre.objects[sourceId];
  if (!source) return null;
  // CR 113.7a: an ability whose source has left the battlefield still
  // resolves; its effects leave from the ability's stack entry.
  const origin: ResolvingOrigin =
    source.zone === "Battlefield"
      ? { zone: "Battlefield", objectId: sourceId, ownerId: source.owner }
      : { zone: "Stack", objectId: resolving.id, ownerId: source.owner };
  return { source, origin };
}

// Blue sources strike with water, except alongside red, whose blue-red spells
// crackle with lightning as colourless ones do; every other colour throws fire.
function causeFor(colors: readonly ManaColor[]): DamageCause {
  if (colors.length === 0) return "lightning";
  if (!colors.includes("Blue")) return "fire";
  return colors.includes("Red") ? "lightning" : "water";
}

/** CR 120.2b: damage dealt as the effect of a spell or ability comes from the
 *  object it names. Damage the resolving spell or ability deals from itself or
 *  its own source travels to its target as a cause. Other damage has none:
 *  combat damage, a mana ability's, or a creature a spell makes deal damage. */
export function damageCauseOf(event: AnimationEvent, pre: GameState | null): DamageCauseOf | null {
  if (event.type !== "DamageDealt" || event.data.is_combat) return null;
  const resolving = resolvingSourceOf(event.data.source_id, pre);
  return resolving && { cause: causeFor(resolving.source.color), origin: resolving.origin };
}
