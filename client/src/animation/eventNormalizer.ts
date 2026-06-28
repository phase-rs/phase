import type { GameEvent } from "../adapter/types";
import type { AnimationStep, PacingCategory, StepEffect } from "./types";
import {
  DEFAULT_DURATION,
  EVENT_DURATIONS,
  GROUPED_COMBAT_DAMAGE_DURATION_MS,
  GROUPED_COMBAT_DAMAGE_THRESHOLD,
  GROUPED_DAMAGE_FLURRY_IMPACT_DELAY_MS,
  defaultPacingMultipliers,
  eventCategory,
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
  "PowerToughnessChanged",
  "CardsDrawn",
  "CardDrawn",
  "PermanentTapped",
  "PermanentUntapped",
  "StackPushed",
  "StackResolved",
  "ReplacementApplied",
  "Regenerated",
  "AttackersDeclared",
  "BlockersDeclared",
  "CombatDamageDealtToPlayer",
  "PlayerCounterChanged",
  // Dice/coin are presented out-of-band by DiceRollOverlay (via flashDiceRoll),
  // not as queued animation steps — same pattern as TurnStarted → the turn banner.
  "DieRolled",
  "StartingPlayerContest",
  "CoinFlipped",
]);

/** Events that always begin a new step, regardless of context. */
const OWN_STEP_TYPES = new Set([
  "SpellCast",
  "TurnStarted",
]);

/** Events that merge into the preceding step rather than starting a new one. */
const MERGE_TYPES = new Set(["ZoneChanged", "LifeChanged"]);

// ---------------------------------------------------------------------------
// Grouping strategies
// ---------------------------------------------------------------------------

type GroupingStrategy = (effect: StepEffect, lastStep: AnimationStep) => boolean;

interface NormalizeEventsOptions {
  /** Per-category pacing multipliers. Each event's category is resolved via
   *  `eventCategory()` and the matching multiplier scales its base duration.
   *  Defaults to neutral pacing (1.0) for every category. */
  pacingMultipliers?: Record<PacingCategory, number>;
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

function toEffect(
  event: GameEvent,
  pacingMultipliers: Record<PacingCategory, number>,
): StepEffect {
  const baseDuration = EVENT_DURATIONS[event.type] ?? DEFAULT_DURATION;
  const multiplier = pacingMultipliers[eventCategory(event.type)];
  return { event, duration: Math.round(baseDuration * multiplier) };
}

function groupedCombatDamageDuration(
  pacingMultipliers: Record<PacingCategory, number>,
): number {
  return Math.max(
    GROUPED_DAMAGE_FLURRY_IMPACT_DELAY_MS,
    Math.round(GROUPED_COMBAT_DAMAGE_DURATION_MS * pacingMultipliers.combat),
  );
}

function stepDuration(effects: StepEffect[]): number {
  return Math.max(...effects.map((e) => e.duration));
}

function isCombatPlayerDamage(
  event: GameEvent,
  playerId?: number,
): event is Extract<GameEvent, { type: "DamageDealt" }> {
  if (event.type !== "DamageDealt") return false;
  if (!event.data.is_combat || !("Player" in event.data.target)) return false;
  return playerId === undefined || event.data.target.Player === playerId;
}

function isDamageLifeLoss(
  event: GameEvent,
  playerId: number,
): event is Extract<GameEvent, { type: "LifeChanged" }> {
  return event.type === "LifeChanged" && event.data.player_id === playerId && event.data.amount < 0;
}

function playerDamageTarget(
  event: Extract<GameEvent, { type: "DamageDealt" }>,
): number {
  if (!("Player" in event.data.target)) {
    throw new Error("Expected player-targeted damage event");
  }
  return event.data.target.Player;
}

interface GroupedReplacement {
  aggregateIndex: number;
  maxMatchedDamageIndex: number;
  skipIndices: Set<number>;
  steps: AnimationStep[];
}

interface AggregateReplacements {
  replacements: GroupedReplacement[];
  fallbackBlockedIndices: Set<number>;
}

type CombatPlayerDamageEvent = Extract<GameEvent, { type: "DamageDealt" }>;

interface DamageIndexEntry {
  index: number;
  event: CombatPlayerDamageEvent;
}

function sourceAmountMap(sourceAmounts: [number, number][]): Map<number, number> {
  const amounts = new Map<number, number>();
  for (const [sourceId, amount] of sourceAmounts) {
    amounts.set(sourceId, (amounts.get(sourceId) ?? 0) + amount);
  }
  return amounts;
}

function isCombatDamageSideEffect(event: GameEvent | undefined): boolean {
  return event?.type === "PlayerCounterChanged";
}

function damageIndexKey(playerId: number, sourceId: number, amount: number): string {
  return `${playerId}:${sourceId}:${amount}`;
}

function buildCombatDamageIndex(
  events: GameEvent[],
  segmentStart: number,
  segmentEnd: number,
): Map<string, DamageIndexEntry[]> {
  const index = new Map<string, DamageIndexEntry[]>();
  for (let eventIndex = segmentStart; eventIndex <= segmentEnd; eventIndex++) {
    const event = events[eventIndex];
    if (!isCombatPlayerDamage(event)) continue;
    const playerId = playerDamageTarget(event);
    const key = damageIndexKey(playerId, event.data.source_id, event.data.amount);
    const entries = index.get(key) ?? [];
    entries.push({ index: eventIndex, event });
    index.set(key, entries);
  }
  return index;
}

function findAdjacentLifeChange(
  events: GameEvent[],
  damageIndex: number,
  segmentStart: number,
  segmentEnd: number,
  consumed: Set<number>,
  playerId: number,
): { lifeIndex: number; sideEffectIndices: number[] } | null {
  const previousSideEffects: number[] = [];
  for (let index = damageIndex - 1; index >= segmentStart; index--) {
    if (consumed.has(index)) continue;
    const event = events[index];
    if (isCombatDamageSideEffect(event)) {
      previousSideEffects.push(index);
      continue;
    }
    if (isDamageLifeLoss(event, playerId)) {
      return { lifeIndex: index, sideEffectIndices: previousSideEffects };
    }
    break;
  }

  const nextSideEffects: number[] = [];
  for (let index = damageIndex + 1; index <= segmentEnd; index++) {
    if (consumed.has(index)) continue;
    const event = events[index];
    if (isCombatDamageSideEffect(event)) {
      nextSideEffects.push(index);
      continue;
    }
    if (isDamageLifeLoss(event, playerId)) {
      return { lifeIndex: index, sideEffectIndices: nextSideEffects };
    }
    break;
  }

  return null;
}

function adjacentCombatSideEffects(
  events: GameEvent[],
  damageIndex: number,
  segmentStart: number,
  segmentEnd: number,
  consumed: Set<number>,
): number[] {
  const sideEffects: number[] = [];
  for (let index = damageIndex - 1; index >= segmentStart; index--) {
    if (consumed.has(index)) continue;
    if (!isCombatDamageSideEffect(events[index])) break;
    sideEffects.push(index);
  }
  for (let index = damageIndex + 1; index <= segmentEnd; index++) {
    if (consumed.has(index)) continue;
    if (!isCombatDamageSideEffect(events[index])) break;
    sideEffects.push(index);
  }
  return sideEffects;
}

function buildGroupedStep(
  playerId: number,
  sourceIds: number[],
  totalDamage: number,
  hitCount: number,
  lifeChanges: Map<number, number>,
  duration: number,
): AnimationStep {
  const effects: StepEffect[] = [
    {
      event: {
        type: "GroupedDamageFlurry",
        data: { player_id: playerId, source_ids: sourceIds, total_damage: totalDamage, hit_count: hitCount },
      },
      duration,
    },
  ];

  for (const [lifePlayerId, amount] of lifeChanges) {
    if (amount === 0) continue;
    effects.push({
      event: { type: "LifeChanged", data: { player_id: lifePlayerId, amount } },
      duration: EVENT_DURATIONS.LifeChanged,
      displayOnly: true,
    });
  }

  return { effects, duration: stepDuration(effects) };
}

function addLifeChange(changes: Map<number, number>, playerId: number, amount: number): void {
  changes.set(playerId, (changes.get(playerId) ?? 0) + amount);
}

function buildLifeChangeStep(lifeChanges: Map<number, number>): AnimationStep {
  const effects: StepEffect[] = [];
  for (const [playerId, amount] of lifeChanges) {
    if (amount === 0) continue;
    effects.push({
      event: { type: "LifeChanged", data: { player_id: playerId, amount } },
      duration: EVENT_DURATIONS.LifeChanged,
      displayOnly: true,
    });
  }
  return { effects, duration: stepDuration(effects) };
}

function contiguousAggregateRunStart(aggregateIndices: number[], aggregatePosition: number): number {
  let runStart = aggregatePosition;
  while (
    runStart > 0 &&
    aggregateIndices[runStart - 1] + 1 === aggregateIndices[runStart]
  ) {
    runStart--;
  }
  return runStart;
}

function findPositiveLifeChanges(
  events: GameEvent[],
  segmentStart: number,
  segmentEnd: number,
  consumed: Set<number>,
): { indices: Set<number>; changes: Map<number, number> } {
  const indices = new Set<number>();
  const changes = new Map<number, number>();
  for (let index = segmentStart; index <= segmentEnd; index++) {
    const event = events[index];
    if (consumed.has(index) || event.type !== "LifeChanged" || event.data.amount <= 0) continue;
    indices.add(index);
    addLifeChange(changes, event.data.player_id, event.data.amount);
  }
  return { indices, changes };
}

function findAggregateReplacements(
  events: GameEvent[],
  pacingMultipliers: Record<PacingCategory, number>,
): AggregateReplacements {
  const aggregateIndices = events.reduce<number[]>((indices, event, index) => {
    if (event.type === "CombatDamageDealtToPlayer") indices.push(index);
    return indices;
  }, []);
  const replacements: GroupedReplacement[] = [];
  const fallbackBlockedIndices = new Set<number>();
  const duration = groupedCombatDamageDuration(pacingMultipliers);

  for (let aggregatePosition = 0; aggregatePosition < aggregateIndices.length; aggregatePosition++) {
    const aggregateIndex = aggregateIndices[aggregatePosition];
    const event = events[aggregateIndex];
    if (event.type !== "CombatDamageDealtToPlayer") continue;

    const sourceAmounts = (event.data.source_amounts ?? []).filter(([, amount]) => amount > 0);
    if (sourceAmounts.length < GROUPED_COMBAT_DAMAGE_THRESHOLD) continue;

    const playerId = event.data.player_id;
    const expectedAmounts = sourceAmountMap(sourceAmounts);
    const matchedDamageIndices = new Set<number>();
    const matchedLifeIndices = new Set<number>();
    const consumedLifeChanges = new Map<number, number>();
    const runStartPosition = contiguousAggregateRunStart(aggregateIndices, aggregatePosition);
    const previousAggregateIndex = aggregateIndices[runStartPosition - 1] ?? -1;
    const segmentStart = previousAggregateIndex + 1;
    const segmentEnd = aggregateIndex - 1;
    const damageIndex = buildCombatDamageIndex(events, segmentStart, segmentEnd);

    for (const [sourceId, amount] of expectedAmounts) {
      const entries = damageIndex.get(damageIndexKey(playerId, sourceId, amount));
      const matched = entries?.shift();
      if (!matched) {
        matchedDamageIndices.clear();
        break;
      }

      const matchedIndex = matched.index;
      matchedDamageIndices.add(matchedIndex);
      const sideEffectIndices = adjacentCombatSideEffects(
        events,
        matchedIndex,
        segmentStart,
        segmentEnd,
        matchedLifeIndices,
      );
      const lifeChange = findAdjacentLifeChange(
        events,
        matchedIndex,
        segmentStart,
        segmentEnd,
        matchedLifeIndices,
        playerId,
      );
      for (const sideEffectIndex of sideEffectIndices) matchedLifeIndices.add(sideEffectIndex);
      if (lifeChange !== null) {
        matchedLifeIndices.add(lifeChange.lifeIndex);
        for (const sideEffectIndex of lifeChange.sideEffectIndices) matchedLifeIndices.add(sideEffectIndex);
        const lifeEvent = events[lifeChange.lifeIndex];
        if (lifeEvent.type === "LifeChanged") {
          addLifeChange(
            consumedLifeChanges,
            lifeEvent.data.player_id,
            lifeEvent.data.amount,
          );
        }
      }
    }

    if (matchedDamageIndices.size !== expectedAmounts.size) {
      for (let index = segmentStart; index < aggregateIndex; index++) {
        fallbackBlockedIndices.add(index);
      }
      continue;
    }

    const skipIndices = new Set<number>([aggregateIndex, ...matchedDamageIndices, ...matchedLifeIndices]);
    replacements.push({
      aggregateIndex,
      maxMatchedDamageIndex: Math.max(...matchedDamageIndices),
      skipIndices,
      steps: [
        buildGroupedStep(
          playerId,
          [...expectedAmounts.keys()],
          event.data.total_damage,
          sourceAmounts.length,
          consumedLifeChanges,
          duration,
        ),
      ],
    });
  }

  const replacementsByAggregate = new Map(replacements.map((replacement) => [replacement.aggregateIndex, replacement]));
  for (let aggregatePosition = 0; aggregatePosition < aggregateIndices.length;) {
    const runStartPosition = aggregatePosition;
    let runEndPosition = aggregatePosition;
    while (
      runEndPosition + 1 < aggregateIndices.length &&
      aggregateIndices[runEndPosition] + 1 === aggregateIndices[runEndPosition + 1]
    ) {
      runEndPosition++;
    }

    const runReplacements = aggregateIndices
      .slice(runStartPosition, runEndPosition + 1)
      .map((index) => replacementsByAggregate.get(index))
      .filter((replacement): replacement is GroupedReplacement => replacement !== undefined);

    if (runReplacements.length > 0) {
      const previousAggregateIndex = aggregateIndices[runStartPosition - 1] ?? -1;
      const segmentStart = previousAggregateIndex + 1;
      const segmentEnd = aggregateIndices[runStartPosition] - 1;
      const lifeWindowStart = Math.max(
        segmentStart,
        Math.max(...runReplacements.map((replacement) => replacement.maxMatchedDamageIndex)) + 1,
      );
      const consumed = new Set<number>();
      for (const replacement of runReplacements) {
        for (const index of replacement.skipIndices) consumed.add(index);
      }
      const positiveLife = findPositiveLifeChanges(events, lifeWindowStart, segmentEnd, consumed);
      if (positiveLife.indices.size > 0) {
        const targetReplacement = runReplacements.length === 1
          ? runReplacements[0]
          : runReplacements[runReplacements.length - 1];
        for (const index of positiveLife.indices) targetReplacement.skipIndices.add(index);
        if (runReplacements.length === 1) {
          const groupedStep = targetReplacement.steps[0];
          for (const [playerId, amount] of positiveLife.changes) {
            if (amount === 0) continue;
            groupedStep.effects.push({
              event: { type: "LifeChanged", data: { player_id: playerId, amount } },
              duration: EVENT_DURATIONS.LifeChanged,
              displayOnly: true,
            });
          }
          groupedStep.duration = stepDuration(groupedStep.effects);
        } else {
          targetReplacement.steps.push(buildLifeChangeStep(positiveLife.changes));
        }
      }
    }

    aggregatePosition = runEndPosition + 1;
  }

  return { replacements, fallbackBlockedIndices };
}

interface FallbackRun {
  nextIndex: number;
  step: AnimationStep;
}

function matchingAdjacentDamageUnit(
  events: GameEvent[],
  index: number,
  playerId: number | null,
): { damage: Extract<GameEvent, { type: "DamageDealt" }>; consumed: number[]; lifeDelta: number } | null {
  const leadingSideEffects: number[] = [];
  let firstIndex = index;

  while (isCombatDamageSideEffect(events[firstIndex])) {
    leadingSideEffects.push(firstIndex);
    firstIndex++;
  }

  const first = events[firstIndex];
  const interveningSideEffects: number[] = [];
  let nextIndex = firstIndex + 1;
  while (isCombatDamageSideEffect(events[nextIndex])) {
    interveningSideEffects.push(nextIndex);
    nextIndex++;
  }
  const next = events[nextIndex];

  if (first?.type === "LifeChanged" && next && isCombatPlayerDamage(next, playerId ?? undefined)) {
    const targetPlayer = playerDamageTarget(next);
    if (isDamageLifeLoss(first, targetPlayer)) {
      return {
        damage: next,
        consumed: [...leadingSideEffects, firstIndex, ...interveningSideEffects, nextIndex],
        lifeDelta: first.data.amount,
      };
    }
  }

  if (first && isCombatPlayerDamage(first, playerId ?? undefined)) {
    const targetPlayer = playerDamageTarget(first);
    const trailingSideEffects: number[] = [];
    let lifeIndex = firstIndex + 1;
    while (
      isCombatDamageSideEffect(events[lifeIndex]) &&
      !isCombatPlayerDamage(events[lifeIndex + 1], playerId ?? undefined)
    ) {
      trailingSideEffects.push(lifeIndex);
      lifeIndex++;
    }
    const lifeEvent = events[lifeIndex];
    const consumed = [...leadingSideEffects, firstIndex, ...trailingSideEffects];
    if (lifeEvent && isDamageLifeLoss(lifeEvent, targetPlayer)) {
      return { damage: first, consumed: [...consumed, lifeIndex], lifeDelta: lifeEvent.data.amount };
    }
    return { damage: first, consumed, lifeDelta: 0 };
  }

  return null;
}

function findFallbackRun(
  events: GameEvent[],
  startIndex: number,
  pacingMultipliers: Record<PacingCategory, number>,
): FallbackRun | null {
  const firstUnit = matchingAdjacentDamageUnit(events, startIndex, null);
  if (!firstUnit) return null;

  const playerId = playerDamageTarget(firstUnit.damage);
  const sourceIds: number[] = [];
  let totalDamage = 0;
  const lifeChanges = new Map<number, number>();
  let hitCount = 0;
  let index = startIndex;

  while (index < events.length) {
    const unit = matchingAdjacentDamageUnit(events, index, playerId);
    if (!unit) break;
    sourceIds.push(unit.damage.data.source_id);
    totalDamage += unit.damage.data.amount;
    if (unit.lifeDelta !== 0) addLifeChange(lifeChanges, playerId, unit.lifeDelta);
    hitCount++;
    index += unit.consumed.length;
  }

  if (hitCount < GROUPED_COMBAT_DAMAGE_THRESHOLD) return null;

  const aggregateIndex = events.findIndex(
    (event, eventIndex) => eventIndex >= index && event.type === "CombatDamageDealtToPlayer",
  );
  const segmentEnd = aggregateIndex === -1 ? index - 1 : aggregateIndex - 1;
  const positiveLife = findPositiveLifeChanges(events, index, segmentEnd, new Set());
  for (const [lifePlayerId, amount] of positiveLife.changes) {
    addLifeChange(lifeChanges, lifePlayerId, amount);
  }

  return {
    nextIndex: positiveLife.indices.size > 0 ? segmentEnd + 1 : index,
    step: buildGroupedStep(
      playerId,
      sourceIds,
      totalDamage,
      hitCount,
      lifeChanges,
      groupedCombatDamageDuration(pacingMultipliers),
    ),
  };
}

// ---------------------------------------------------------------------------
// Main normalizer
// ---------------------------------------------------------------------------

export function normalizeEvents(
  events: GameEvent[],
  options?: NormalizeEventsOptions,
): AnimationStep[] {
  const pacingMultipliers = options?.pacingMultipliers ?? defaultPacingMultipliers();
  const steps: AnimationStep[] = [];
  const { replacements: aggregateReplacements, fallbackBlockedIndices } = findAggregateReplacements(events, pacingMultipliers);
  const replacementByAggregateIndex = new Map(
    aggregateReplacements.map((replacement) => [replacement.aggregateIndex, replacement]),
  );
  const skipIndices = new Set<number>();
  for (const replacement of aggregateReplacements) {
    for (const index of replacement.skipIndices) skipIndices.add(index);
  }

  for (let index = 0; index < events.length; index++) {
    if (skipIndices.has(index)) {
      const replacement = replacementByAggregateIndex.get(index);
      if (replacement) steps.push(...replacement.steps);
      continue;
    }

    const fallbackRun = fallbackBlockedIndices.has(index)
      ? null
      : findFallbackRun(events, index, pacingMultipliers);
    if (fallbackRun) {
      steps.push(fallbackRun.step);
      index = fallbackRun.nextIndex - 1;
      continue;
    }

    const event = events[index];
    if (NON_VISUAL_EVENTS.has(event.type)) continue;

    const effect = toEffect(event, pacingMultipliers);

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
