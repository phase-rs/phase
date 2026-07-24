import { useMemo } from "react";

import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import type { ObjectId } from "../../adapter/types.ts";

/**
 * Status of an attacker's minimum-blocker requirement, derived from the
 * engine-provided `block_requirements` (CR 702.111b Menace / CR 509.1b) and the
 * player's in-progress assignments:
 * - `pending`    — not yet blocked (legal: "or not at all").
 * - `incomplete` — blocked by 1+ but fewer than required.
 * - `satisfied`  — blocked by at least the required count.
 */
export type BlockRequirementStatus = "pending" | "incomplete" | "satisfied";

export interface BlockRequirement {
  attackerId: ObjectId;
  required: number;
  assigned: number;
  status: BlockRequirementStatus;
  /** CR 702.111b / CR 509.1b: permanents imposing the min-blocker floor. */
  sources: ObjectId[];
}

export interface BlockRequirements {
  byAttacker: Map<ObjectId, BlockRequirement>;
}

const EMPTY: BlockRequirements = { byAttacker: new Map() };

/**
 * Compares the engine-declared per-attacker minimum-blocker requirements against
 * the player's current assignments. The requirement values come entirely from
 * the engine (`DeclareBlockers.block_requirements`); this only counts the user's
 * own pending selections against them — no game-rules logic lives here.
 */
export function useBlockRequirements(): BlockRequirements {
  const blockRequirements = useGameStore((s) =>
    s.waitingFor?.type === "DeclareBlockers" ? s.waitingFor.data.block_requirements : undefined,
  );
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);

  return useMemo(() => {
    if (!blockRequirements || Object.keys(blockRequirements).length === 0) {
      return EMPTY;
    }

    // Count the UI's selected pairs for presentation only. The engine remains
    // the authority on whether a declaration is legal or requirement-maximal.
    const assignedPerAttacker = new Map<ObjectId, number>();
    for (const attackerIds of blockerAssignments.values()) {
      for (const attackerId of attackerIds) {
        assignedPerAttacker.set(attackerId, (assignedPerAttacker.get(attackerId) ?? 0) + 1);
      }
    }

    const byAttacker = new Map<ObjectId, BlockRequirement>();
    for (const [attackerKey, requirement] of Object.entries(blockRequirements)) {
      const attackerId = Number(attackerKey);
      const required = requirement.count;
      const assigned = assignedPerAttacker.get(attackerId) ?? 0;
      const status: BlockRequirementStatus =
        assigned === 0 ? "pending" : assigned < required ? "incomplete" : "satisfied";
      byAttacker.set(attackerId, {
        attackerId,
        required,
        assigned,
        status,
        sources: requirement.sources ?? [],
      });
    }

    return { byAttacker };
  }, [blockRequirements, blockerAssignments]);
}
