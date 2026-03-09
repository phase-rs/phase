export type VfxQuality = "full" | "reduced" | "minimal";

export type AnimationSpeed = "slow" | "normal" | "fast" | "instant";

export const SPEED_MULTIPLIERS: Record<AnimationSpeed, number> = {
  slow: 1.5,
  normal: 1.0,
  fast: 0.5,
  instant: 0,
};

export interface StepEffect {
  type: string;
  data: unknown;
  duration: number;
}

export interface AnimationStep {
  effects: StepEffect[];
  duration: number;
}

export type PositionSnapshot = Map<number, DOMRect>;

export const EVENT_DURATIONS: Record<string, number> = {
  ZoneChanged: 400,
  DamageDealt: 300,
  LifeChanged: 300,
  SpellCast: 500,
  CreatureDestroyed: 400,
  TokenCreated: 400,
  CounterAdded: 200,
  CounterRemoved: 200,
  PermanentTapped: 200,
  PermanentUntapped: 200,
  AttackersDeclared: 300,
  BlockersDeclared: 300,
};

export const DEFAULT_DURATION = 200;
