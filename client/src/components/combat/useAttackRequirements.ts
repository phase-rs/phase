import { useMemo } from "react";

import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import type { CombatRequirement, ObjectId } from "../../adapter/types.ts";

/**
 * Per-creature attacker-requirement status, derived from the engine-provided
 * `attacker_constraints` (CR 508.1c must-attack / can't-attack) and the player's
 * in-progress attacker selection:
 * - `pending`   — a MustAttack creature not yet selected (illegal to confirm).
 * - `satisfied` — a MustAttack creature currently selected.
 * - `info`      — a CantAttack creature (informational; can never be selected).
 */
export type AttackRequirementStatus = "pending" | "satisfied" | "info";

export interface AttackRequirement {
  objectId: ObjectId;
  kind: CombatRequirement["kind"];
  status: AttackRequirementStatus;
}

export interface AttackRequirements {
  byObject: Map<ObjectId, AttackRequirement>;
  /** MustAttack creatures not yet selected — confirmation must be blocked. */
  unsatisfiedMustAttackCount: number;
}

const EMPTY: AttackRequirements = { byObject: new Map(), unsatisfiedMustAttackCount: 0 };

/**
 * Compares the engine-declared per-creature attacker constraints against the
 * player's current selection. All constraint values come entirely from the
 * engine (`DeclareAttackers.attacker_constraints`); this only counts the user's
 * own pending selections against them — no game-rules logic lives here.
 */
export function useAttackRequirements(): AttackRequirements {
  const attackerConstraints = useGameStore((s) =>
    s.waitingFor?.type === "DeclareAttackers" ? s.waitingFor.data.attacker_constraints : undefined,
  );
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);

  return useMemo(() => {
    if (!attackerConstraints || Object.keys(attackerConstraints).length === 0) {
      return EMPTY;
    }

    const selected = new Set(selectedAttackers);
    const byObject = new Map<ObjectId, AttackRequirement>();
    let unsatisfiedMustAttackCount = 0;

    for (const [key, requirement] of Object.entries(attackerConstraints)) {
      const objectId = Number(key);
      if (requirement.kind === "MustAttack") {
        const status: AttackRequirementStatus = selected.has(objectId) ? "satisfied" : "pending";
        if (status === "pending") unsatisfiedMustAttackCount += 1;
        byObject.set(objectId, { objectId, kind: requirement.kind, status });
      } else if (requirement.kind === "CantAttack") {
        byObject.set(objectId, { objectId, kind: requirement.kind, status: "info" });
      }
    }

    return { byObject, unsatisfiedMustAttackCount };
  }, [attackerConstraints, selectedAttackers]);
}
