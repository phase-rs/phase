import { useMemo } from "react";

import { useGameStore } from "../../stores/gameStore.ts";
import type { CombatRequirement, ObjectId } from "../../adapter/types.ts";

/**
 * Per-creature blocker-constraint status, derived from the engine-provided
 * `blocker_constraints` (CR 509.1c must-block / CR 509.1b can't-block) and the
 * engine-provided constraints:
 * - `pending`   — a MustBlock requirement the engine still needs to validate.
 * - `info`      — a CantBlock creature (informational; can never be assigned).
 */
export type BlockerConstraintStatus = "pending" | "info";

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
 * engine (`DeclareBlockers.blocker_constraints`). It deliberately does not infer
 * that a local assignment satisfies a requirement: CR 509.1c validation belongs
 * exclusively to the engine.
 */
export function useBlockerConstraints(): BlockerConstraints {
  const blockerConstraints = useGameStore((s) =>
    s.waitingFor?.type === "DeclareBlockers" ? s.waitingFor.data.blocker_constraints : undefined,
  );
  return useMemo(() => {
    if (!blockerConstraints || Object.keys(blockerConstraints).length === 0) {
      return EMPTY;
    }

    const byObject = new Map<ObjectId, BlockerConstraint>();

    for (const [key, requirement] of Object.entries(blockerConstraints)) {
      const objectId = Number(key);
      if (requirement.kind === "MustBlock") {
        const requiredAttackers = requirement.attackers ?? [];
        byObject.set(objectId, {
          objectId,
          kind: requirement.kind,
          status: "pending",
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
  }, [blockerConstraints]);
}
