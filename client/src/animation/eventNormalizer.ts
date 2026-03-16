import type { GameEvent } from "../adapter/types";
import type { AnimationStep, StepEffect } from "./types";
import {
  COMBAT_PACING_MULTIPLIERS,
  DEFAULT_DURATION,
  EVENT_DURATIONS,
  type CombatPacing,
} from "./types";

// ---------------------------------------------------------------------------
// Step classification sets
// ---------------------------------------------------------------------------

/** Events that produce no visual output and are skipped entirely. */
const NON_VISUAL_EVENTS = new Set([
  "PriorityPassed",
  "MulliganStarted",
  "GameStarted",
  "ManaAdded",
  "DamageCleared",
  "CardsDrawn",
  "CardDrawn",
  "PermanentTapped",
  "PermanentUntapped",
  "StackPushed",
  "StackResolved",
  "ReplacementApplied",
  "AttackersDeclared",
]);

/** Events that always begin a new step, regardless of context. */
const OWN_STEP_TYPES = new Set([
  "SpellCast",
  "TurnStarted",
  "BlockersDeclared",
]);

/** Events that merge into the preceding step rather than starting a new one. */
const MERGE_TYPES = new Set(["ZoneChanged", "LifeChanged"]);

// ---------------------------------------------------------------------------
// Grouping strategies
// ---------------------------------------------------------------------------

type GroupingStrategy = (effect: StepEffect, lastStep: AnimationStep) => boolean;

interface NormalizeEventsOptions {
  combatPacing?: CombatPacing;
}

/** Group consecutive events of the same type (e.g. multiple creatures dying). */
function sameTypeGrouping(effect: StepEffect, lastStep: AnimationStep): boolean {
  return lastStep.effects[lastStep.effects.length - 1]?.event.type === effect.event.type;
}

/**
 * Finds an existing step that this DamageDealt event is the bidirectional pair of
 * (i.e., source and Object target are swapped), indicating a single attacker↔blocker
 * engagement. Scans all steps because the engine emits all attacker assignments
 * before any blocker assignments, so pairs are never adjacent in the event stream.
 */
function findCombatPairStep(
  effect: StepEffect,
  steps: AnimationStep[],
): AnimationStep | null {
  if (effect.event.type !== "DamageDealt") return null;
  const { source_id, target } = effect.event.data;
  if (!("Object" in target)) return null;

  for (const step of steps) {
    for (const e of step.effects) {
      if (e.event.type !== "DamageDealt") continue;
      const prevTarget = e.event.data.target;
      if (
        e.event.data.source_id === target.Object &&
        "Object" in prevTarget &&
        prevTarget.Object === source_id
      ) {
        return step;
      }
    }
  }
  return null;
}

/**
 * Maps event types to their grouping strategy.
 * To add a new grouping behavior, register it here.
 */
const GROUPING_STRATEGIES: Map<string, GroupingStrategy> = new Map([
  ["CreatureDestroyed", sameTypeGrouping],
  ["PermanentSacrificed", sameTypeGrouping],
]);

// ---------------------------------------------------------------------------
// Step construction helpers
// ---------------------------------------------------------------------------

const COMBAT_PACED_EVENT_TYPES = new Set(["BlockersDeclared", "DamageDealt"]);

function toEffect(event: GameEvent, combatPacing: CombatPacing): StepEffect {
  const baseDuration = EVENT_DURATIONS[event.type] ?? DEFAULT_DURATION;
  if (!COMBAT_PACED_EVENT_TYPES.has(event.type)) {
    return { event, duration: baseDuration };
  }

  const pacedDuration = Math.round(baseDuration * COMBAT_PACING_MULTIPLIERS[combatPacing]);
  return { event, duration: pacedDuration };
}

function stepDuration(effects: StepEffect[]): number {
  return Math.max(...effects.map((e) => e.duration));
}

// ---------------------------------------------------------------------------
// Main normalizer
// ---------------------------------------------------------------------------

export function normalizeEvents(
  events: GameEvent[],
  options?: NormalizeEventsOptions,
): AnimationStep[] {
  const combatPacing = options?.combatPacing ?? "normal";
  const steps: AnimationStep[] = [];

  for (const event of events) {
    if (NON_VISUAL_EVENTS.has(event.type)) continue;

    const effect = toEffect(event, combatPacing);

    if (OWN_STEP_TYPES.has(event.type)) {
      steps.push({ effects: [effect], duration: effect.duration });
      continue;
    }

    if (MERGE_TYPES.has(event.type) && steps.length > 0) {
      const lastStep = steps[steps.length - 1];
      lastStep.effects.push(effect);
      lastStep.duration = stepDuration(lastStep.effects);
      continue;
    }

    // DamageDealt: pair attacker↔blocker into the same step regardless of position,
    // since the engine emits all attacker assignments before all blocker assignments.
    const pairStep = findCombatPairStep(effect, steps);
    if (pairStep) {
      pairStep.effects.push(effect);
      pairStep.duration = stepDuration(pairStep.effects);
      continue;
    }

    const grouping = GROUPING_STRATEGIES.get(event.type);
    if (grouping && steps.length > 0 && grouping(effect, steps[steps.length - 1])) {
      const lastStep = steps[steps.length - 1];
      lastStep.effects.push(effect);
      lastStep.duration = stepDuration(lastStep.effects);
      continue;
    }

    steps.push({ effects: [effect], duration: effect.duration });
  }

  return steps;
}
