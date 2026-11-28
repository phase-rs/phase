import {
  impactDelayMsForAnimationEvent,
  lifeChangeImpactDelayMs,
  type AnimationStep,
  type StepEffect,
} from "./types";

/**
 * SFX the presenter fires at its own impact frame — the card slam and the
 * damage flurry — rather than when the beat starts. The overlay's Classic and
 * card-VFX paths both play these from their `onImpact` callback, so a beat
 * must not play them a second time.
 */
const SLAM_DEFERRED_SFX: Readonly<Record<string, true>> = {
  DamageDealt: true,
  GroupedDamageFlurry: true,
};

/** One beat of the Director's timeline. */
export interface Beat {
  /** Informational; unique only within one compile call. */
  id: string;
  /** Before contact. Phase 0 beats start at contact. */
  anticipationMs: number;
  /** When this beat's hit lands, from the existing impact helpers, clamped to the beat. */
  impactAtMs: number;
  /** After contact: the remainder of the beat. */
  settleMs: number;
  /** Total wall-clock at pace 1. Phase 0: exactly the source step's duration. */
  durationMs: number;
  /** The step this beat presents, 1:1 in Phase 0. */
  step: AnimationStep;
  /** Step SFX played when the beat becomes active; slam-deferred events are excluded. */
  sfx: readonly StepEffect[];
}

/**
 * Compile one normalized step into one beat.
 *
 * Phase 0 is deliberately 1:1 so presentation looks the same as today: the
 * beat lasts exactly the step's duration and has no settle cues, so the next
 * beat starts when this one ends (today's overlay timer). Impact and settle
 * are recorded for the beat's `impactAtMs` consumers and for Phase 1, but the
 * overlay keeps computing its own cause-aware impact until then.
 */
export function compileBeat(step: AnimationStep, index = 0): Beat {
  const impactAtMs = beatImpactMs(step);
  return {
    id: `beat:${index}`,
    anticipationMs: 0,
    impactAtMs,
    settleMs: Math.max(0, step.duration - impactAtMs),
    durationMs: step.duration,
    step,
    sfx: step.effects.filter(
      (effect) => !effect.displayOnly && !SLAM_DEFERRED_SFX[effect.event.type],
    ),
  };
}

/** Compile one action's steps into its timeline, one step to one beat. */
export function compileStepsToBeats(steps: readonly AnimationStep[]): Beat[] {
  return steps.map((step, index) => compileBeat(step, index));
}

/**
 * The beat's contact frame. A step may present several simultaneous effects
 * (a hit and the life change it causes), so the last of their impacts is the
 * frame everything shares. `lifeChangeImpactDelayMs` already resolves a life
 * change against the hit in the same step.
 */
function beatImpactMs(step: AnimationStep): number {
  let impact = 0;
  for (const effect of step.effects) {
    const event = effect.event;
    const delay = event.type === "LifeChanged"
      ? lifeChangeImpactDelayMs(effect, step.effects, event.data.player_id)
      : impactDelayMsForAnimationEvent(event);
    if (delay > impact) impact = delay;
  }
  return Math.min(impact, step.duration);
}
