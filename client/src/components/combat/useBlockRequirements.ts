import { useMemo } from "react";

import { useGameStore } from "../../stores/gameStore.ts";
import type { ObjectId } from "../../adapter/types.ts";

/**
 * Engine-provided minimum-blocker requirement (CR 702.111b Menace / CR 509.1b).
 * The engine alone determines whether an in-progress declaration is legal.
 */
export interface BlockRequirement {
  attackerId: ObjectId;
  required: number;
  /** CR 702.111b / CR 509.1b: permanents imposing the min-blocker floor. */
  sources: ObjectId[];
}

export interface BlockRequirements {
  byAttacker: Map<ObjectId, BlockRequirement>;
}

const EMPTY: BlockRequirements = { byAttacker: new Map() };

/**
 * Returns the engine-declared per-attacker minimum-blocker requirements.
 */
export function useBlockRequirements(): BlockRequirements {
  const blockRequirements = useGameStore((s) =>
    s.waitingFor?.type === "DeclareBlockers" ? s.waitingFor.data.block_requirements : undefined,
  );

  return useMemo(() => {
    if (!blockRequirements || Object.keys(blockRequirements).length === 0) {
      return EMPTY;
    }

    const byAttacker = new Map<ObjectId, BlockRequirement>();
    for (const [attackerKey, requirement] of Object.entries(blockRequirements)) {
      const attackerId = Number(attackerKey);
      byAttacker.set(attackerId, {
        attackerId,
        required: requirement.count,
        sources: requirement.sources ?? [],
      });
    }

    return { byAttacker };
  }, [blockRequirements]);
}
