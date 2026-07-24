import { useMemo } from "react";

import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import type { CombatRequirement, ObjectId } from "../../adapter/types.ts";

/**
 * Per-creature blocker-constraint status, derived from the engine-provided
 * `blocker_constraints` (CR 509.1c must-block / CR 509.1b can't-block) and the
 * player's in-progress blocker assignments:
 * - `pending`   — a MustBlock creature lacks a matching selected assignment.
 * - `satisfied` — generic MustBlock has any assignment; exact MustBlock has
 *                 every engine-provided required attacker selected.
 * - `info`      — a CantBlock creature (informational; can never be assigned).
 */
export type BlockerConstraintStatus = "pending" | "satisfied" | "info";

export interface BlockerConstraint {
  objectId: ObjectId;
  kind: CombatRequirement["kind"];
  status: BlockerConstraintStatus;
  /** Engine-provided objects imposing this constraint (CR 509.1b/c). */
  sources: ObjectId[];
  /** Engine-provided exact attackers this creature is asked to block. */
  attackers: ObjectId[];
}

export interface BlockerConstraints {
  byObject: Map<ObjectId, BlockerConstraint>;
}

const EMPTY: BlockerConstraints = { byObject: new Map() };

/**
 * Compares the engine-declared per-creature blocker constraints against the
 * player's current assignments. All constraint values come entirely from the
 * engine (`DeclareBlockers.blocker_constraints`); this only counts the user's own
 * in-progress assignments against them — no game-rules logic lives here.
 */
export function useBlockerConstraints(): BlockerConstraints {
  const blockerConstraints = useGameStore((s) =>
    s.waitingFor?.type === "DeclareBlockers" ? s.waitingFor.data.blocker_constraints : undefined,
  );
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);

  return useMemo(() => {
    if (!blockerConstraints || Object.keys(blockerConstraints).length === 0) {
      return EMPTY;
    }

    // This only annotates the player's selection. The engine decides whether it
    // satisfies every requirement and whether it can be declared.
    const byObject = new Map<ObjectId, BlockerConstraint>();

    for (const [key, requirement] of Object.entries(blockerConstraints)) {
      const objectId = Number(key);
      if (requirement.kind === "MustBlock") {
        const requiredAttackers = requirement.attackers ?? [];
        const selectedAttackers = blockerAssignments.get(objectId);
        const status: BlockerConstraintStatus = selectedAttackers != null
          && (requiredAttackers.length === 0
            || requiredAttackers.every((attackerId) => selectedAttackers.has(attackerId)))
          ? "satisfied"
          : "pending";
        byObject.set(objectId, {
          objectId,
          kind: requirement.kind,
          status,
          sources: requirement.sources ?? [],
          attackers: requiredAttackers,
        });
      } else if (requirement.kind === "CantBlock") {
        byObject.set(objectId, {
          objectId,
          kind: requirement.kind,
          status: "info",
          sources: requirement.sources ?? [],
          attackers: [],
        });
      }
    }

    return { byObject };
  }, [blockerConstraints, blockerAssignments]);
}
