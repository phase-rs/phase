import type { GameEvent } from "../adapter/types";
import type { AnimationStep, StepEffect } from "./types";
import { DEFAULT_DURATION, EVENT_DURATIONS } from "./types";

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
]);

/** Event types that group with consecutive events of the same type */
const GROUPABLE_TYPES = new Set([
  "DamageDealt",
  "CreatureDestroyed",
  "PermanentSacrificed",
]);

/** Event types that always start their own step */
const OWN_STEP_TYPES = new Set([
  "SpellCast",
  "TurnStarted",
  "BlockersDeclared",
]);

/** Event types that merge into the preceding step rather than starting a new one */
const MERGE_TYPES = new Set(["ZoneChanged", "LifeChanged"]);

function toEffect(event: GameEvent): StepEffect {
  return {
    type: event.type,
    data: "data" in event ? event.data : undefined,
    duration: EVENT_DURATIONS[event.type] ?? DEFAULT_DURATION,
  };
}

function stepDuration(effects: StepEffect[]): number {
  return Math.max(...effects.map((e) => e.duration));
}

export function normalizeEvents(events: GameEvent[]): AnimationStep[] {
  const steps: AnimationStep[] = [];

  for (const event of events) {
    if (NON_VISUAL_EVENTS.has(event.type)) {
      continue;
    }

    // Split AttackersDeclared into one step per attacker for staggered animation.
    // Each step fires VFX (burst + projectile) for a single attacker sequentially.
    if (event.type === "AttackersDeclared") {
      const duration = EVENT_DURATIONS["AttackersDeclared"] ?? DEFAULT_DURATION;
      const { attacker_ids, defending_player } = event.data as {
        attacker_ids: number[];
        defending_player: number;
      };
      for (const attackerId of attacker_ids) {
        steps.push({
          effects: [{ type: "AttackersDeclared", data: { attacker_ids: [attackerId], defending_player }, duration }],
          duration,
        });
      }
      continue;
    }

    const effect = toEffect(event);

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

    if (GROUPABLE_TYPES.has(event.type) && steps.length > 0) {
      const lastStep = steps[steps.length - 1];
      const lastEffect = lastStep.effects[lastStep.effects.length - 1];
      if (lastEffect.type === event.type) {
        lastStep.effects.push(effect);
        lastStep.duration = stepDuration(lastStep.effects);
        continue;
      }
    }

    steps.push({ effects: [effect], duration: effect.duration });
  }

  return steps;
}
