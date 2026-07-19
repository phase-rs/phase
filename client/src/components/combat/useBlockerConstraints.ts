import { useMemo } from "react";

import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import type { CombatRequirement, ObjectId } from "../../adapter/types.ts";

/**
 * Per-creature blocker-constraint status, derived from the engine-provided
 * `blocker_constraints` (CR 509.1c must-block / CR 509.1b can't-block) and the
 * player's in-progress blocker assignments:
 * - `pending`   — a MustBlock creature not yet assigned to any attacker (illegal
 *                 to confirm). The engine already excludes must-block creatures
 *                 with zero legal targets, so no target check is needed here.
 * - `satisfied` — a MustBlock creature currently assigned.
 * - `info`      — a CantBlock creature (informational; can never be assigned).
 */
export type BlockerConstraintStatus = "pending" | "satisfied" | "info";

export interface BlockerConstraint {
  objectId: ObjectId;
  kind: CombatRequirement["kind"];
  status: BlockerConstraintStatus;
  /** Engine-provided objects imposing this constraint (CR 509.1b/c). */
  sources: ObjectId[];
}

export interface BlockerConstraints {
  byObject: Map<ObjectId, BlockerConstraint>;
  /** MustBlock creatures not yet assigned — confirmation must be blocked. */
  unsatisfiedMustBlockCount: number;
}

const EMPTY: BlockerConstraints = { byObject: new Map(), unsatisfiedMustBlockCount: 0 };

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

    // blockerAssignments maps blockerId -> attackerId; a must-block creature is
    // satisfied once it appears as a key (assigned to some attacker).
    const byObject = new Map<ObjectId, BlockerConstraint>();
    let unsatisfiedMustBlockCount = 0;

    for (const [key, requirement] of Object.entries(blockerConstraints)) {
      const objectId = Number(key);
      if (requirement.kind === "MustBlock") {
        const status: BlockerConstraintStatus = blockerAssignments.has(objectId)
          ? "satisfied"
          : "pending";
        if (status === "pending") unsatisfiedMustBlockCount += 1;
        byObject.set(objectId, {
          objectId,
          kind: requirement.kind,
          status,
          sources: requirement.sources ?? [],
        });
      } else if (requirement.kind === "CantBlock") {
        byObject.set(objectId, {
          objectId,
          kind: requirement.kind,
          status: "info",
          sources: requirement.sources ?? [],
        });
      }
    }

    return { byObject, unsatisfiedMustBlockCount };
  }, [blockerConstraints, blockerAssignments]);
}
