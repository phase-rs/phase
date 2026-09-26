import type { GameObject } from "../../adapter/types.ts";

export const TABLETOP_FLYING_HOVER_LIFT = 0.065;
export const TABLETOP_FLYING_BOB_AMPLITUDE = 0.03;
export const TABLETOP_FLYING_BOB_PERIOD_SECONDS = 3.2;

/**
 * Presentation eligibility comes directly from the engine-projected permanent:
 * only a creature that currently has Flying receives the airborne treatment.
 */
export function isFlyingCreature(
  object: Pick<GameObject, "card_types" | "keywords"> | null | undefined,
): boolean {
  return object?.card_types.core_types.includes("Creature") === true
    && object.keywords.includes("Flying");
}

/** A deterministic phase keeps several flyers from moving in lockstep. */
export function tabletopFlyingBobOffset(
  elapsedSeconds: number,
  objectId: number,
  animationSpeedMultiplier: number,
): number {
  if (animationSpeedMultiplier <= 0) return 0;

  const angularVelocity = (Math.PI * 2)
    / (TABLETOP_FLYING_BOB_PERIOD_SECONDS * animationSpeedMultiplier);
  const phase = (Math.abs(objectId) % 11) * 0.61;
  // Keep the entire bob above the normal permanent resting plane. Letting the
  // sine wave cross below that plane pushes the card's cast shadow into the
  // tabletop at the trough, where it clips and appears to blink out.
  return TABLETOP_FLYING_HOVER_LIFT
    + Math.sin(elapsedSeconds * angularVelocity + phase)
      * TABLETOP_FLYING_BOB_AMPLITUDE;
}
