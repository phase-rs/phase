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
 * Group DamageDealt events into per-attacker engagements.
 * Blockers fighting the same attacker share a step via participant overlap;
 * each new attacker starts its own step.
 */
function engagementGrouping(effect: StepEffect, lastStep: AnimationStep): boolean {
  if (effect.event.type !== "DamageDealt") return false;
  if (!lastStep.effects.some((e) => e.event.type === "DamageDealt")) return false;

  const participants = new Set<number>();
  for (const e of lastStep.effects) {
    if (e.event.type !== "DamageDealt") continue;
    const { source_id, target } = e.event.data;
    participants.add(source_id);
    if ("Object" in target) participants.add(target.Object);
  }

  const { source_id, target } = effect.event.data;
  return participants.has(source_id) || ("Object" in target && participants.has(target.Object));
}

/**
 * Maps event types to their grouping strategy.
 * To add a new grouping behavior, register it here.
 */
const GROUPING_STRATEGIES: Map<string, GroupingStrategy> = new Map([
  ["CreatureDestroyed", sameTypeGrouping],
  ["PermanentSacrificed", sameTypeGrouping],
  ["DamageDealt", engagementGrouping],
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
