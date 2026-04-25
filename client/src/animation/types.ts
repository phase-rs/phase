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

export const PACING_LABELS: Record<PacingCategory, string> = {
  effects: "Effect Pacing",
  combat: "Combat Pacing",
  banners: "Banner Pacing",
};

export const PACING_DESCRIPTIONS: Record<PacingCategory, string> = {
  effects: "Spell casts, zone changes, deaths, life changes, counters, tap/untap.",
  combat: "Combat damage timing — how long blockers and attackers linger before damage resolves.",
  banners: "Turn-start banner display.",
};

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
};

export function eventCategory(eventType: string): PacingCategory {
  return EVENT_PACING_CATEGORY[eventType] ?? "effects";
}

export interface StepEffect {
  event: GameEvent;
  duration: number;
}

export interface AnimationStep {
  effects: StepEffect[];
  duration: number;
}

export type PositionSnapshot = Map<number, DOMRect>;

/** Combat pacing defaults (normal speed). */
export const COMBAT_ENGAGEMENT_DURATION_MS = 900;

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

/** Base "your turn / opponent's turn" banner display duration, before any
 *  pacing multipliers apply. */
export const TURN_BANNER_DURATION_MS = 1500;
