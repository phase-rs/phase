import { useCallback, useMemo, useState } from "react";
import { motion, useReducedMotion } from "framer-motion";
import { Trans, useTranslation } from "react-i18next";

import type { AttackTarget, GameObject, ObjectId, PlayerId } from "../../adapter/types.ts";
import { getSeatColor } from "../../hooks/useSeatColor.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { formatCounterType } from "../../viewmodel/cardProps.ts";
import {
  type AttackerStack,
  attackTargetKey,
  attackTargetsForAttacker,
  commonAttackTargets,
  evenSplit,
  groupAttackers,
} from "../../utils/combat.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { PeekTab } from "../modal/DialogShell.tsx";
import { CounterTooltip } from "../ui/CounterTooltip.tsx";

/** Internal assignment map: every attacker maps to its chosen target, or `null`
 *  while it sits in the Unassigned bucket. */
type AssignmentMap = Map<ObjectId, AttackTarget | null>;

interface AttackTargetPickerProps {
  /**
   * Aggregate compatibility target list (union of every attacker's legal
   * targets). Used for display ordering and as the legacy fallback when the
   * engine supplies no per-attacker map.
   */
  validTargets: AttackTarget[];
  /**
   * Engine-authoritative per-attacker legal targets (`DeclareAttackers`
   * `valid_attack_targets_by_attacker`, keyed by stringified ObjectId).
   * `undefined` for a legacy payload → fall back to `validTargets`. When
   * present, each attacker may only be aimed at its own bucket (CR 508.1c
   * scoped restrictions are already baked into the map by the engine), so the
   * picker is pure presentation over engine choices — no client legality.
   */
  validTargetsByAttacker?: Record<string, AttackTarget[]>;
  selectedAttackers: ObjectId[];
  onConfirm: (attacks: [ObjectId, AttackTarget][]) => void;
  onCancel: () => void;
}

/**
 * Attack-target selection for multiplayer / multi-defender games.
 *
 * Two modes:
 * - "all" (default): pick one target, all attackers go there.
 * - "distribute": a bucket-per-target board where identical attackers are
 *   grouped into stacks (via the shared `groupAttackers` building block) and
 *   spread across every valid target plus an Unassigned bucket. Per-target
 *   counts are tuned with +/- steppers; whole-stack and even-split shortcuts
 *   fill buckets fast. Confirm stays disabled until Unassigned is empty, since
 *   every declared attacker must be given a target.
 *
 * Frontend display layer only: it merely arranges the attacker→target choices
 * the player makes and hands the flat array to the engine, which validates it.
 *
 * Layout mirrors {@link DialogShell}: a `relative` wrapper hosts the `PeekTab`
 * as a sibling OUTSIDE the `overflow-hidden` card, and the card is a flex column
 * with a pinned header, a single scrollable body, and a pinned footer. Keeping
 * the tab out of the scroll container is what stops its ~12px `translate-x-1/3`
 * overhang from forcing a stray horizontal scrollbar; making the body the only
 * scroll region keeps the actions on-screen and any needed scrollbar thin.
 */
export function AttackTargetPicker({
  validTargets,
  validTargetsByAttacker,
  selectedAttackers,
  onConfirm,
  onCancel,
}: AttackTargetPickerProps) {
  const { t } = useTranslation("game");
  const [mode, setMode] = useState<"all" | "distribute">("all");
  const [peeked, setPeeked] = useState(false);
  // Every attacker starts in the Unassigned bucket (null). Keyed by attacker
  // ObjectId so a stack's members can land on different targets.
  const [assignments, setAssignments] = useState<AssignmentMap>(
    () => new Map(selectedAttackers.map((id) => [id, null] as const)),
  );
  const [expandedStack, setExpandedStack] = useState<string | null>(null);
  const shouldReduceMotion = useReducedMotion();

  const gameState = useGameStore((s) => s.gameState);
  const playerNames = useMultiplayerStore((s) => s.playerNames);
  const myId = usePlayerId();
  const hoverProps = useInspectHoverProps();
  const seatOrder = gameState?.seat_order;
  const teamBased = gameState?.format_config?.team_based ?? false;

  const sortTargets = useCallback(
    (targets: AttackTarget[]): AttackTarget[] => {
      if (!seatOrder) return targets;
      return [...targets].sort((a, b) => {
        const aIdx = a.type === "Player" ? seatOrder.indexOf(a.data) : Infinity;
        const bIdx = b.type === "Player" ? seatOrder.indexOf(b.data) : Infinity;
        if (aIdx !== bIdx) return aIdx - bIdx;
        // Total order: two non-Player targets both map to Infinity (as do any equal
        // seat-index ties), so tie-break on the numeric id. Without this the
        // comparator returns `Infinity - Infinity === NaN` for a pair of
        // planeswalkers/battles, leaving their order — and thus which defender takes
        // the front-loaded even-split remainder — dependent on JS sort stability.
        return Number(a.data) - Number(b.data);
      });
    },
    [seatOrder],
  );

  // Per-attacker legal targets: engine-authoritative map, or the aggregate list
  // for a legacy payload. Pure presentation over engine choices — no client
  // legality is computed here.
  const targetsFor = useCallback(
    (id: ObjectId) => sortTargets(attackTargetsForAttacker(id, validTargetsByAttacker, validTargets)),
    [sortTargets, validTargetsByAttacker, validTargets],
  );

  // "Attack All" offers only targets EVERY selected attacker may legally attack
  // (the intersection of their engine-provided legal sets, CR 508.1c).
  const attackAllTargets = useMemo(
    () => sortTargets(commonAttackTargets(selectedAttackers, validTargetsByAttacker, validTargets)),
    [sortTargets, selectedAttackers, validTargetsByAttacker, validTargets],
  );

  // Stacks of identical attackers, reusing the battlefield grouping block, then
  // split so identically-named attackers with different legal-target sets are
  // not treated as one interchangeable stack.
  const stacks = useMemo(
    () => groupAttackers(selectedAttackers, gameState, targetsFor),
    [selectedAttackers, gameState, targetsFor],
  );

  // Distribute-mode columns: the union of legal targets across the selected
  // attackers (a stack's row only exposes steppers for its own bucket).
  const distributeColumns = useMemo(() => {
    const seen = new Map<string, AttackTarget>();
    for (const stack of stacks) {
      for (const target of stack.targets) seen.set(attackTargetKey(target), target);
    }
    return sortTargets([...seen.values()]);
  }, [stacks, sortTargets]);

  // The board state supplies each creature's current evaluated power. This is
  // intentionally only an unblocked, at-this-moment life estimate: blockers
  // and later game actions still determine the actual combat result.
  const assignedDamageByPlayer = useMemo(() => {
    const damageByPlayer = new Map<PlayerId, number>();
    for (const attackerId of selectedAttackers) {
      const target = assignments.get(attackerId);
      if (target?.type !== "Player") continue;
      const damage = Math.max(0, gameState?.objects[attackerId]?.power ?? 0);
      damageByPlayer.set(target.data, (damageByPlayer.get(target.data) ?? 0) + damage);
    }
    return damageByPlayer;
  }, [assignments, gameState, selectedAttackers]);

  // Total attackers still in the Unassigned bucket — gates Confirm.
  const unassignedTotal = useMemo(
    () => selectedAttackers.reduce((n, id) => n + (assignments.get(id) == null ? 1 : 0), 0),
    [assignments, selectedAttackers],
  );

  function getTargetLabel(target: AttackTarget, showProjectedLife = false): string {
    if (target.type === "Player") {
      const life = gameState?.players.find((player) => player.id === target.data)?.life;
      const name = target.data === myId
        ? t("attackTargetPicker.you")
        : teamBased && Math.floor(target.data / 2) === Math.floor(myId / 2)
          ? t("attackTargetPicker.ally")
          : playerNames.get(target.data) ?? `Opp ${target.data + 1}`;
      const currentLife = life ?? 0;
      const assignedDamage = showProjectedLife
        ? (assignedDamageByPlayer.get(target.data) ?? 0)
        : 0;
      if (assignedDamage > 0) {
        const projectedLife = Math.max(0, currentLife - assignedDamage);
        return projectedLife === 0
          ? t("attackTargetPicker.playerTargetLethal", {
            name,
            life: currentLife,
            projectedLife,
          })
          : t("attackTargetPicker.playerTargetProjected", {
            name,
            life: currentLife,
            projectedLife,
          });
      }
      return t("attackTargetPicker.playerTarget", {
        name,
        life: currentLife,
      });
    }
    const obj = gameState?.objects[target.data];
    const name = obj?.name ?? t("attackTargetPicker.objectFallback", { id: target.data });
    if (target.type === "Planeswalker") {
      return t("attackTargetPicker.planeswalkerTarget", { name });
    }
    if (target.type === "Battle") {
      return t("attackTargetPicker.battleTarget", { name });
    }
    return name;
  }

  function getTargetSeatColor(target: AttackTarget): string | undefined {
    if (target.type === "Player") {
      return getSeatColor(target.data, seatOrder);
    }
    const obj = gameState?.objects[target.data];
    return obj ? getSeatColor(obj.controller, seatOrder) : undefined;
  }

  function handleAttackAll(target: AttackTarget) {
    // `attackAllTargets` is already the intersection of every selected
    // attacker's engine-provided legal set, so any offered target is legal for
    // all of them (CR 508.1c). No client legality re-check.
    onConfirm(selectedAttackers.map((id) => [id, target]));
  }

  // --- Distribute-mode assignment mutations (all clone-then-mutate; pure
  // transforms live at module scope for deterministic, testable moves). ---

  function mutate(fn: (next: AssignmentMap) => void) {
    setAssignments((prev) => {
      const next = new Map(prev);
      fn(next);
      return next;
    });
  }

  /** +1: claim the lowest-id Unassigned member of the stack for this target. */
  function incOnTarget(stack: AttackerStack, target: AttackTarget) {
    mutate((next) => {
      const id = lowestUnassigned(stack, next);
      if (id != null) next.set(id, target);
    });
  }

  /** -1: release the highest-id member currently on this target back to Unassigned. */
  function decFromTarget(stack: AttackerStack, target: AttackTarget) {
    mutate((next) => {
      const id = highestOnTarget(stack, target, next);
      if (id != null) next.set(id, null);
    });
  }

  /** Send the entire stack to one target (overrides members already elsewhere). */
  function allOfStackToTarget(stack: AttackerStack, target: AttackTarget) {
    mutate((next) => {
      for (const id of stack.ids) next.set(id, target);
    });
  }

  /** Spread one stack evenly across its own legal targets. */
  function spreadStack(stack: AttackerStack) {
    mutate((next) => spreadStackEvenly(next, stack, stack.targets));
  }

  /** Spread every selected attacker evenly across the legal targets. When every
   *  stack shares one legal set (the common case, incl. legacy payloads) this is
   *  a single global even split across the shared columns; when legal sets differ
   *  (CR 508.1c scoped restrictions) each stack is split only within its own
   *  engine-provided bucket so no attacker lands on an illegal target. */
  function spreadAll() {
    mutate((next) => {
      const distinctSets = new Set(
        stacks.map((s) => s.targets.map(attackTargetKey).sort().join("|")),
      );
      if (distinctSets.size <= 1 && distributeColumns.length > 0) {
        const allIds = stacks.flatMap((s) => s.ids).sort((a, b) => a - b);
        spreadAttackersEvenly(next, allIds, distributeColumns);
      } else {
        for (const stack of stacks) spreadAttackersEvenly(next, stack.ids, stack.targets);
      }
    });
  }

  /** Send every attacker that may legally attack `target` to it (stacks whose
   *  engine-provided bucket excludes `target` are left untouched, CR 508.1c). */
  function allStacksToTarget(target: AttackTarget) {
    const key = attackTargetKey(target);
    mutate((next) => {
      for (const stack of stacks) {
        if (!stack.targets.some((tt) => attackTargetKey(tt) === key)) continue;
        for (const id of stack.ids) next.set(id, target);
      }
    });
  }

  /** Return every attacker to the Unassigned bucket. */
  function resetAll() {
    mutate((next) => {
      for (const id of selectedAttackers) next.set(id, null);
    });
  }

  function countOnTarget(stack: AttackerStack, target: AttackTarget): number {
    const key = attackTargetKey(target);
    return stack.ids.reduce((n, id) => {
      const t = assignments.get(id);
      return n + (t && attackTargetKey(t) === key ? 1 : 0);
    }, 0);
  }

  function countUnassigned(stack: AttackerStack): number {
    return stack.ids.reduce((n, id) => n + (assignments.get(id) == null ? 1 : 0), 0);
  }

  function handleDistributeConfirm() {
    // The button is disabled while anything is unassigned; guard here too so a
    // stray call can't submit an incomplete set. Target legality is the engine's
    // job — every stepper only offered engine-provided targets (CR 508.1c).
    if (unassignedTotal > 0) return;
    // The gate guarantees no nulls, but flatMap also makes the types sound.
    const attacks = selectedAttackers.flatMap((id): [ObjectId, AttackTarget][] => {
      const target = assignments.get(id);
      return target ? [[id, target]] : [];
    });
    onConfirm(attacks);
  }

  const slideTransform = peeked
    ? { x: "calc(100vw - 32px)" }
    : { x: 0 };

  const sidePadding = mode === "all" ? "px-8" : "px-4 sm:px-6";

  return (
    <>
      <motion.div
        className="fixed inset-0 z-40 flex items-center justify-center bg-black/60 p-3"
        style={{ pointerEvents: peeked ? "none" : undefined }}
        animate={slideTransform}
        transition={
          shouldReduceMotion
            ? { duration: 0 }
            : { type: "spring", stiffness: 320, damping: 32 }
        }
      >
        {/* Wrapper: positioning context for the PeekTab, which sits at the
            wrapper's right edge OUTSIDE the card. The card clips its own overflow
            (overflow-hidden), so the tab's ~12px `translate-x-1/3` overhang no
            longer forces a stray horizontal scrollbar the way it did when the tab
            lived inside the scroll container. Mirrors DialogShell. `w-full` +
            `max-w-*` lets the card shrink to fit narrow phones instead of forcing
            a fixed 420px that would overflow. */}
        <div
          className={`relative w-full ${mode === "all" ? "max-w-[420px]" : "max-w-[760px]"}`}
        >
          <div className="flex max-h-[85vh] flex-col overflow-hidden rounded-xl border border-gray-600 bg-gray-900/95 shadow-2xl backdrop-blur-sm">
            {/* Header — pinned, never scrolls */}
            <div className={`shrink-0 pt-5 ${sidePadding}`}>
              <h3 className="mb-4 text-center text-lg font-bold text-gray-100">
                {t("attackTargetPicker.heading")}
              </h3>

              {/* Mode toggle */}
              <div className="flex justify-center gap-2">
                <button
                  onClick={() => setMode("all")}
                  className={gameButtonClass({
                    tone: mode === "all" ? "blue" : "slate",
                    size: "sm",
                  })}
                >
                  {t("attackTargetPicker.attackAll")}
                </button>
                <button
                  onClick={() => setMode("distribute")}
                  className={gameButtonClass({
                    tone: mode === "distribute" ? "blue" : "slate",
                    size: "sm",
                  })}
                >
                  {t("attackTargetPicker.distribute")}
                </button>
              </div>
            </div>

            {/* Body — the ONLY scroll region. `overflow-x-hidden` pins the cross
                axis so a marginally-wide child can't sprout the very horizontal
                scrollbar this layout removes (the desktop matrix keeps its own
                `overflow-x-auto`); `overscroll-contain` stops a mobile scroll from
                chaining to the board behind; `thin-scrollbar` keeps any needed
                scrollbar unobtrusive. */}
            <div className={`min-h-0 flex-1 overflow-y-auto overflow-x-hidden overscroll-contain thin-scrollbar pb-2 pt-4 ${sidePadding}`}>
              {mode === "all" ? (
                /* Attack All mode: one button per target every selected attacker
                   can legally attack (the engine-provided intersection). */
                <div className="flex flex-col gap-2">
                  {attackAllTargets.length === 0 ? (
                    <p className="px-1 py-4 text-center text-xs font-medium text-amber-300">
                      {t("attackTargetPicker.noCommonTarget")}
                    </p>
                  ) : (
                    attackAllTargets.map((target) => {
                      const color = getTargetSeatColor(target);
                      return (
                        <div key={attackTargetKey(target)} className="flex flex-col gap-0.5">
                          <button
                            onClick={() => handleAttackAll(target)}
                            className={gameButtonClass({ tone: "red", size: "md" })}
                          >
                            <Trans
                              t={t}
                              i18nKey="attackTargetPicker.attackWith"
                              count={selectedAttackers.length}
                              values={{ label: getTargetLabel(target), count: selectedAttackers.length }}
                              components={{
                                name: <span className="mx-1 font-bold" style={color ? { color } : undefined} />,
                              }}
                            />
                          </button>
                        </div>
                      );
                    })
                  )}
                </div>
              ) : (
                /* Distribute mode: per-target buckets with steppers + shortcuts */
                <div className="flex flex-col gap-3">
                  {/* Global shortcuts + gate hint */}
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <p className={`text-xs font-medium ${unassignedTotal > 0 ? "text-amber-300" : "text-emerald-300"}`}>
                      {unassignedTotal > 0
                        ? t("attackTargetPicker.unassignedRemaining", { count: unassignedTotal })
                        : t("attackTargetPicker.allAssigned")}
                    </p>
                    <div className="flex flex-wrap gap-1.5">
                      <button
                        onClick={spreadAll}
                        disabled={distributeColumns.length === 0}
                        className={gameButtonClass({ tone: "indigo", size: "xs", disabled: distributeColumns.length === 0 })}
                      >
                        {t("attackTargetPicker.evenSplitAll")}
                      </button>
                      <button
                        onClick={resetAll}
                        disabled={unassignedTotal === selectedAttackers.length}
                        className={gameButtonClass({ tone: "slate", size: "xs", disabled: unassignedTotal === selectedAttackers.length })}
                      >
                        {t("attackTargetPicker.resetAssignments")}
                      </button>
                    </div>
                  </div>

                  {/* Desktop: stacks (rows) × buckets (columns) matrix */}
                  <div className="hidden overflow-x-auto overscroll-x-contain thin-scrollbar md:block">
                    <table className="w-full border-separate border-spacing-0 text-sm">
                      <thead>
                        <tr>
                          <th className="sticky left-0 z-10 bg-gray-900 px-2 py-1.5 text-left text-xs font-semibold text-gray-400">
                            {t("attackTargetPicker.attackersColumn")}
                          </th>
                          <th className="px-2 py-1.5 text-center text-xs font-semibold text-gray-400">
                            {t("attackTargetPicker.unassigned")}
                          </th>
                          {distributeColumns.map((target) => {
                            const color = getTargetSeatColor(target);
                            return (
                              <th key={attackTargetKey(target)} className="px-2 py-1.5 text-center align-top">
                                <div className="flex flex-col items-center gap-1">
                                  <span
                                    className="inline-flex items-center gap-1 text-xs font-semibold"
                                    style={color ? { color } : undefined}
                                  >
                                    <span className="inline-block h-2 w-2 rounded-full" style={{ backgroundColor: color ?? "#6b7280" }} />
                                    <span className="max-w-[7rem] truncate">{getTargetLabel(target, true)}</span>
                                  </span>
                                  <button
                                    type="button"
                                    onClick={() => allStacksToTarget(target)}
                                    className="rounded border border-gray-600 px-1.5 py-0.5 text-[10px] font-medium text-gray-300 hover:border-gray-400 hover:bg-white/10"
                                  >
                                    {t("attackTargetPicker.allHere")}
                                  </button>
                                </div>
                              </th>
                            );
                          })}
                        </tr>
                      </thead>
                      <tbody>
                        {stacks.map((stack) => {
                          const unassigned = countUnassigned(stack);
                          const legalKeys = new Set(stack.targets.map(attackTargetKey));
                          return (
                            <tr key={stack.key} className="border-t border-white/5">
                              <td className="sticky left-0 z-10 bg-gray-900 px-2 py-1.5">
                                <div className="flex items-center gap-2">
                                  <StackLabel stack={stack} t={t} hoverProps={hoverProps} />
                                  <button
                                    type="button"
                                    onClick={() => spreadStack(stack)}
                                    disabled={stack.targets.length === 0}
                                    title={t("attackTargetPicker.spreadEvenly")}
                                    className="ml-auto shrink-0 rounded border border-gray-600 px-1.5 py-0.5 text-[10px] font-medium text-gray-300 hover:border-gray-400 hover:bg-white/10 disabled:opacity-30"
                                  >
                                    {t("attackTargetPicker.spread")}
                                  </button>
                                </div>
                              </td>
                              <td className="px-2 py-1.5 text-center">
                                <span
                                  className={`inline-block min-w-[1.5rem] rounded px-1.5 py-0.5 text-sm font-semibold tabular-nums ${
                                    unassigned > 0 ? "bg-amber-900/60 text-amber-100" : "text-gray-600"
                                  }`}
                                >
                                  {unassigned}
                                </span>
                              </td>
                              {distributeColumns.map((target) => {
                                // A stack whose engine-provided bucket excludes this
                                // target gets an inert cell — the attacker can't be
                                // aimed there (CR 508.1c).
                                if (!legalKeys.has(attackTargetKey(target))) {
                                  return (
                                    <td key={attackTargetKey(target)} className="px-2 py-1.5 text-center text-gray-700">
                                      —
                                    </td>
                                  );
                                }
                                const count = countOnTarget(stack, target);
                                const label = getTargetLabel(target, true);
                                return (
                                  <td key={attackTargetKey(target)} className="px-2 py-1.5">
                                    <StepperCell
                                      count={count}
                                      color={getTargetSeatColor(target)}
                                      canDec={count > 0}
                                      canInc={unassigned > 0}
                                      onDec={() => decFromTarget(stack, target)}
                                      onInc={() => incOnTarget(stack, target)}
                                      onAll={() => allOfStackToTarget(stack, target)}
                                      decTitle={t("attackTargetPicker.removeOne", { label })}
                                      incTitle={t("attackTargetPicker.assignOne", { label })}
                                      allTitle={t("attackTargetPicker.assignAllHere", { label })}
                                    />
                                  </td>
                                );
                              })}
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                  </div>

                  {/* Mobile: per-stack accordion driving the same assignment state */}
                  <div className="flex flex-col gap-2 md:hidden">
                    {stacks.map((stack) => {
                      const unassigned = countUnassigned(stack);
                      const expanded = expandedStack === stack.key;
                      return (
                        <div key={stack.key} className="overflow-hidden rounded-lg border border-gray-700">
                          <button
                            type="button"
                            onClick={() => setExpandedStack((cur) => (cur === stack.key ? null : stack.key))}
                            aria-expanded={expanded}
                            className="flex w-full items-center gap-2 px-2 py-2.5 text-left hover:bg-white/5"
                          >
                            <StackLabel stack={stack} t={t} hoverProps={hoverProps} />
                            <span
                              className={`ml-auto shrink-0 rounded px-1.5 py-0.5 text-[10px] font-bold ${
                                unassigned > 0 ? "bg-amber-900/70 text-amber-100" : "bg-emerald-900/70 text-emerald-100"
                              }`}
                            >
                              {unassigned > 0
                                ? t("attackTargetPicker.unassignedRemaining", { count: unassigned })
                                : t("attackTargetPicker.assignedBadge")}
                            </span>
                            <svg
                              xmlns="http://www.w3.org/2000/svg"
                              viewBox="0 0 16 16"
                              fill="currentColor"
                              className={`h-3.5 w-3.5 shrink-0 text-gray-400 transition-transform ${expanded ? "rotate-180" : ""}`}
                            >
                              <path fillRule="evenodd" d="M4.22 6.22a.75.75 0 0 1 1.06 0L8 8.94l2.72-2.72a.75.75 0 1 1 1.06 1.06l-3.25 3.25a.75.75 0 0 1-1.06 0L4.22 7.28a.75.75 0 0 1 0-1.06Z" clipRule="evenodd" />
                            </svg>
                          </button>
                          {expanded && (
                            <div className="flex flex-col gap-1.5 border-t border-white/10 px-2 py-2">
                              <button
                                type="button"
                                onClick={() => spreadStack(stack)}
                                disabled={stack.targets.length === 0}
                                className={`self-start ${gameButtonClass({ tone: "indigo", size: "xs", disabled: stack.targets.length === 0 })}`}
                              >
                                {t("attackTargetPicker.spreadEvenly")}
                              </button>
                              <div className="flex items-center justify-between gap-2 rounded px-1 py-1">
                                <span className="text-sm text-gray-400">{t("attackTargetPicker.unassigned")}</span>
                                <span
                                  className={`min-w-[1.5rem] rounded px-1.5 py-0.5 text-center text-sm font-semibold tabular-nums ${
                                    unassigned > 0 ? "bg-amber-900/60 text-amber-100" : "text-gray-600"
                                  }`}
                                >
                                  {unassigned}
                                </span>
                              </div>
                              {stack.targets.map((target) => {
                                const color = getTargetSeatColor(target);
                                const count = countOnTarget(stack, target);
                                const label = getTargetLabel(target, true);
                                return (
                                  <div key={attackTargetKey(target)} className="flex items-center justify-between gap-2 rounded px-1 py-1">
                                    <span className="inline-flex min-w-0 items-center gap-1.5 text-sm" style={color ? { color } : undefined}>
                                      <span className="inline-block h-2.5 w-2.5 shrink-0 rounded-full" style={{ backgroundColor: color ?? "#6b7280" }} />
                                      <span className="truncate">{label}</span>
                                    </span>
                                    <StepperCell
                                      count={count}
                                      color={color}
                                      canDec={count > 0}
                                      canInc={unassigned > 0}
                                      onDec={() => decFromTarget(stack, target)}
                                      onInc={() => incOnTarget(stack, target)}
                                      onAll={() => allOfStackToTarget(stack, target)}
                                      decTitle={t("attackTargetPicker.removeOne", { label })}
                                      incTitle={t("attackTargetPicker.assignOne", { label })}
                                      allTitle={t("attackTargetPicker.assignAllHere", { label })}
                                    />
                                  </div>
                                );
                              })}
                            </div>
                          )}
                        </div>
                      );
                    })}
                  </div>
                </div>
              )}
            </div>

            {/* Footer — pinned actions, so Confirm/Cancel never scroll away */}
            <div className={`shrink-0 border-t border-white/10 pb-5 pt-3 ${sidePadding}`}>
              {mode === "distribute" && (
                <>
                  <button
                    onClick={handleDistributeConfirm}
                    disabled={unassignedTotal > 0}
                    className={`w-full ${gameButtonClass({ tone: "emerald", size: "md", disabled: unassignedTotal > 0 })}`}
                  >
                    {unassignedTotal > 0
                      ? t("attackTargetPicker.assignRemaining", { count: unassignedTotal })
                      : t("attackTargetPicker.confirmDistribute", { count: selectedAttackers.length })}
                  </button>
                </>
              )}
              <button
                onClick={onCancel}
                className={`w-full ${mode === "distribute" ? "mt-2" : ""} ${gameButtonClass({ tone: "slate", size: "sm" })}`}
              >
                {t("common:actions.cancel")}
              </button>
            </div>
          </div>
          <PeekTab onClick={() => setPeeked(true)} />
        </div>
      </motion.div>
      {peeked && <RestoreTab onClick={() => setPeeked(false)} />}
    </>
  );
}

function objectPtLabel(obj: GameObject | undefined): string | null {
  if (obj?.power == null || obj.toughness == null) return null;
  return `${obj.power}/${obj.toughness}`;
}

function objectCounterChips(obj: GameObject | undefined): Array<{ type: string; count: number }> {
  if (!obj) return [];
  return Object.entries(obj.counters)
    .filter((entry): entry is [string, number] => entry[1] != null && entry[1] > 0 && entry[0] !== "loyalty")
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([type, count]) => ({ type, count }));
}

function RestoreTab({ onClick }: { onClick: () => void }) {
  const { t } = useTranslation("game");
  return (
    <motion.button
      type="button"
      onClick={onClick}
      aria-label={t("attackTargetPicker.restoreDialog")}
      title={t("attackTargetPicker.restoreDialogTitle")}
      initial={{ opacity: 0, scale: 0.9 }}
      animate={{
        opacity: 1,
        scale: 1,
        boxShadow: [
          "0 18px 36px rgba(0,0,0,0.45), 0 0 0 1px rgba(34,211,238,0.2)",
          "0 18px 36px rgba(0,0,0,0.45), 0 0 28px rgba(34,211,238,0.55)",
          "0 18px 36px rgba(0,0,0,0.45), 0 0 0 1px rgba(34,211,238,0.2)",
        ],
      }}
      transition={{
        opacity: { delay: 0.1, duration: 0.2 },
        scale: { delay: 0.1, duration: 0.2 },
        boxShadow: { duration: 2.4, repeat: Infinity, ease: "easeInOut" },
      }}
      className="fixed right-3 top-1/2 z-[60] flex h-24 w-9 -translate-y-1/2 items-center justify-center rounded-2xl border border-cyan-400/40 bg-[#0b1020]/96 text-cyan-200 backdrop-blur-md transition-colors hover:bg-cyan-500/20 hover:text-white"
    >
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 20 20"
        fill="currentColor"
        className="h-6 w-6 rotate-180"
      >
        <path
          fillRule="evenodd"
          d="M7.22 4.22a.75.75 0 0 1 1.06 0l5.25 5.25a.75.75 0 0 1 0 1.06l-5.25 5.25a.75.75 0 1 1-1.06-1.06L11.94 10 7.22 5.28a.75.75 0 0 1 0-1.06Z"
          clipRule="evenodd"
        />
      </svg>
    </motion.button>
  );
}

// --- Pure assignment transforms (deterministic; mutate the passed map). ---

/** Lowest-id member of the stack currently in the Unassigned bucket, or null. */
function lowestUnassigned(stack: AttackerStack, map: AssignmentMap): ObjectId | null {
  for (const id of stack.ids) {
    if (map.get(id) == null) return id;
  }
  return null;
}

/** Highest-id member of the stack currently assigned to `target`, or null. */
function highestOnTarget(stack: AttackerStack, target: AttackTarget, map: AssignmentMap): ObjectId | null {
  const key = attackTargetKey(target);
  for (let i = stack.ids.length - 1; i >= 0; i--) {
    const t = map.get(stack.ids[i]);
    if (t && attackTargetKey(t) === key) return stack.ids[i];
  }
  return null;
}

/**
 * Redistribute a whole stack evenly across `targets` (overrides prior
 * assignments). Members are walked in ascending-id order and handed to targets
 * in display order, with the remainder front-loaded by {@link evenSplit}.
 */
function spreadStackEvenly(map: AssignmentMap, stack: AttackerStack, targets: AttackTarget[]): void {
  spreadAttackersEvenly(map, stack.ids, targets);
}

/**
 * Redistribute attackers evenly across `targets` (overrides prior assignments).
 * Attackers are walked in stable UI order and handed to targets in display
 * order, with the remainder front-loaded by {@link evenSplit}.
 */
function spreadAttackersEvenly(map: AssignmentMap, attackerIds: ObjectId[], targets: AttackTarget[]): void {
  if (targets.length === 0) return;
  const counts = evenSplit(attackerIds.length, targets.length);
  let member = 0;
  targets.forEach((target, ti) => {
    for (let k = 0; k < counts[ti]; k++) {
      map.set(attackerIds[member], target);
      member += 1;
    }
  });
}

interface StepperCellProps {
  count: number;
  color?: string;
  canInc: boolean;
  canDec: boolean;
  onDec: () => void;
  onInc: () => void;
  onAll: () => void;
  decTitle: string;
  incTitle: string;
  allTitle: string;
}

/** `[ − ] N [ + ]` for one (stack, bucket) cell. The count doubles as a button
 *  that sends the whole stack to this bucket. Buttons are enlarged below `md`
 *  (the breakpoint where the mobile accordion — not the compact desktop matrix —
 *  is shown) for comfortable touch targets without widening the desktop table. */
function StepperCell({ count, color, canInc, canDec, onDec, onInc, onAll, decTitle, incTitle, allTitle }: StepperCellProps) {
  return (
    <div className="flex items-center justify-center gap-1">
      <button
        type="button"
        onClick={onDec}
        disabled={!canDec}
        title={decTitle}
        aria-label={decTitle}
        className="flex h-11 w-11 items-center justify-center rounded border border-gray-600 text-lg leading-none text-gray-200 hover:border-gray-400 hover:bg-white/10 disabled:cursor-default disabled:opacity-30 md:h-6 md:w-6 md:text-base"
      >
        −
      </button>
      <button
        type="button"
        onClick={onAll}
        title={allTitle}
        aria-label={allTitle}
        className={`min-w-[2.75rem] rounded px-1 py-2.5 text-center text-sm font-semibold tabular-nums hover:bg-white/10 md:min-w-[1.9rem] md:py-0.5 ${count > 0 ? "text-gray-100" : "text-gray-500"}`}
        style={count > 0 && color ? { color } : undefined}
      >
        {count}
      </button>
      <button
        type="button"
        onClick={onInc}
        disabled={!canInc}
        title={incTitle}
        aria-label={incTitle}
        className="flex h-11 w-11 items-center justify-center rounded border border-gray-600 text-lg leading-none text-gray-200 hover:border-gray-400 hover:bg-white/10 disabled:cursor-default disabled:opacity-30 md:h-6 md:w-6 md:text-base"
      >
        +
      </button>
    </div>
  );
}

interface StackLabelProps {
  stack: AttackerStack;
  t: ReturnType<typeof useTranslation>["t"];
  hoverProps: ReturnType<typeof useInspectHoverProps>;
}

/** Stack name + count badge + P/T + counter chips, with inspect-on-hover. */
function StackLabel({ stack, t, hoverProps }: StackLabelProps) {
  const ptLabel = objectPtLabel(stack.representative ?? undefined);
  const counters = objectCounterChips(stack.representative ?? undefined);
  return (
    <div className="min-w-0" {...hoverProps(stack.ids[0])}>
      <div className="flex min-w-0 items-center gap-1.5">
        <span className="truncate text-sm font-medium text-gray-100">
          {stack.name || t("attackTargetPicker.creatureFallback", { id: stack.ids[0] })}
        </span>
        {stack.count > 1 && (
          <span className="shrink-0 rounded bg-gray-700 px-1 text-[10px] font-bold text-gray-100">
            ×{stack.count}
          </span>
        )}
        {ptLabel && (
          <span className="shrink-0 rounded bg-amber-900/80 px-1 text-[10px] font-bold text-amber-100">
            {ptLabel}
          </span>
        )}
      </div>
      {counters.length > 0 && (
        <div className="mt-0.5 flex flex-wrap gap-1">
          {counters.map(({ type, count }) => (
            <CounterTooltip key={type} type={type} count={count}>
              <span className="rounded bg-sky-900/80 px-1 text-[10px] font-semibold text-sky-100">
                {formatCounterType(type)} x{count}
              </span>
            </CounterTooltip>
          ))}
        </div>
      )}
    </div>
  );
}
