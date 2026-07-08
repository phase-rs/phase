import type { GameEvent } from "../adapter/types";

export type VfxQuality = "full" | "reduced" | "minimal";

/** Continuous animation-speed multiplier. `0` short-circuits the wait entirely
 *  (the legacy "instant" mode). Values above 1 slow things down; below 1 speed
 *  things up. The slider in settings exposes this directly. */
export const ANIMATION_SPEED_DEFAULT = 1.0;
export const ANIMATION_SPEED_MIN = 0;
export const ANIMATION_SPEED_MAX = 2;
export const ANIMATION_SPEED_STEP = 0.05;

/** Per-category pacing multipliers applied to event durations *before* the
 *  global animation-speed multiplier. The `category()` lookup below routes
 *  every animated event into exactly one of these buckets. */
export type PacingCategory = "effects" | "combat" | "banners";

export const PACING_CATEGORIES: readonly PacingCategory[] = ["effects", "combat", "banners"] as const;

// Per-category labels/descriptions are frontend-authored display text and are
// translated at the render site via `t("pacing.labels.<category>")` /
// `t("pacing.descriptions.<category>")` (settings namespace).

export const PACING_DEFAULT = 1.0;
export const PACING_MIN = 0;
export const PACING_MAX = 2;
export const PACING_STEP = 0.05;

export function defaultPacingMultipliers(): Record<PacingCategory, number> {
  return { effects: PACING_DEFAULT, combat: PACING_DEFAULT, banners: PACING_DEFAULT };
}

/** Maps an event type to its pacing category. Anything not listed falls into
 *  `"effects"`. Keep the table sparse — only events that need a non-default
 *  category appear here. */
const EVENT_PACING_CATEGORY: Record<string, PacingCategory> = {
  DamageDealt: "combat",
  GroupedDamageFlurry: "combat",
};

export function eventCategory(eventType: string): PacingCategory {
  return EVENT_PACING_CATEGORY[eventType] ?? "effects";
}

export type GroupedDamageFlurryEvent = {
  type: "GroupedDamageFlurry";
  data: {
    player_id: number;
    source_ids: number[];
    total_damage: number;
    hit_count: number;
  };
};

export type AnimationEvent = GameEvent | GroupedDamageFlurryEvent;

export interface StepEffect {
  event: AnimationEvent;
  duration: number;
  displayOnly?: true;
}

export interface AnimationStep {
  effects: StepEffect[];
  duration: number;
}

export type PositionSnapshot = Map<number, DOMRect>;

/** Combat pacing defaults (normal speed). */
export const COMBAT_ENGAGEMENT_DURATION_MS = 900;
export const GROUPED_COMBAT_DAMAGE_THRESHOLD = 12;
export const GROUPED_COMBAT_DAMAGE_DURATION_MS = 900;
export const GROUPED_DAMAGE_FLURRY_IMPACT_DELAY_MS = 260;
export const DAMAGE_FLURRY_SOURCE_SAMPLE_LIMIT = 16;
export const DAMAGE_FLURRY_PROJECTILE_MIN = 8;
export const DAMAGE_FLURRY_PROJECTILE_MAX = 32;
export const DAMAGE_FLURRY_TRAIL_PARTICLE_MAX = 96;
export const GROUPED_EVENT_RUN_THRESHOLD = 8;
export const GROUPED_TOKEN_CREATION_THRESHOLD = GROUPED_EVENT_RUN_THRESHOLD;

export const EVENT_DURATIONS: Record<string, number> = {
  ZoneChanged: 400,
  DamageDealt: COMBAT_ENGAGEMENT_DURATION_MS,
  LifeChanged: 300,
  SpellCast: 500,
  CreatureDestroyed: 400,
  TokenCreated: 400,
  CounterAdded: 200,
  CounterRemoved: 200,
  PermanentTapped: 200,
  PermanentUntapped: 200,
};

export const DEFAULT_DURATION = 200;

/** How long the card slam flight phase takes before impact (ms, before speed multiplier). */
export const CARD_SLAM_FLIGHT_MS = 200;

export function isPlayerDamageAnimationEvent(event: AnimationEvent, playerId: number): boolean {
  if (event.type === "GroupedDamageFlurry") {
    return event.data.player_id === playerId;
  }
  return event.type === "DamageDealt" && "Player" in event.data.target && event.data.target.Player === playerId;
}

export function impactDelayMsForAnimationEvent(event: AnimationEvent): number {
  if (event.type === "GroupedDamageFlurry") return GROUPED_DAMAGE_FLURRY_IMPACT_DELAY_MS;
  if (event.type === "DamageDealt" && "Player" in event.data.target) return CARD_SLAM_FLIGHT_MS;
  return 0;
}

/** Base "your turn / opponent's turn" banner display duration, before any
 *  pacing multipliers apply. */
export const TURN_BANNER_DURATION_MS = 1500;

/** Base dice-roll overlay display duration (the 3D tumble + settle + a beat to
 *  read the result), before pacing multipliers. The tumble itself is ~1.6s
 *  inside Dice3D; this is the total time the overlay stays mounted. */
export const DICE_ROLL_DURATION_MS = 2400;
