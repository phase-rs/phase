import { useState } from "react";

import type { DamageAssignment } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";

export function DamageAssignmentModal() {
  const combat = useGameStore((s) => s.gameState?.combat ?? null);
  const objects = useGameStore((s) => s.gameState?.objects ?? null);
  const phase = useGameStore((s) => s.gameState?.phase ?? null);
  const [open, setOpen] = useState(false);

  const isCombatDamage = phase === "CombatDamage";
  const hasDamage =
    combat !== null &&
    Object.keys(combat.damage_assignments).length > 0;

  if (!isCombatDamage || !hasDamage) return null;

  const getName = (id: number): string =>
    objects?.[id]?.name ?? `Object ${id}`;

  if (!open) {
    return (
      <button
        onClick={() => setOpen(true)}
        className={gameButtonClass({ tone: "slate", size: "xs" }) + " fixed bottom-20 right-4 z-30"}
      >
        Review Damage
      </button>
    );
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" />
      <div className="relative z-10 w-full max-w-md rounded-xl bg-gray-900/95 p-6 shadow-2xl ring-1 ring-gray-700">
        <h2 className="mb-4 text-center text-lg font-bold text-white">
          Combat Damage Distribution
        </h2>

        <div className="mb-4 space-y-3">
          {Object.entries(combat!.damage_assignments).map(
            ([attackerIdStr, assignments]: [string, DamageAssignment[]]) => {
              const attackerId = Number(attackerIdStr);
              return (
                <div key={attackerIdStr} className="rounded-lg bg-gray-800/60 p-3">
                  <p className="mb-1 text-sm font-semibold text-amber-300">
                    {getName(attackerId)}
                  </p>
                  <ul className="space-y-0.5 text-sm text-gray-300">
                    {assignments.map((a, i) => {
                      const targetName =
                        "Object" in a.target
                          ? getName(a.target.Object)
                          : `Player ${a.target.Player}`;
                      return (
                        <li key={i}>
                          {a.amount} damage to {targetName}
                        </li>
                      );
                    })}
                  </ul>
                </div>
              );
            },
          )}
        </div>

        <div className="flex justify-center">
          <button
            onClick={() => setOpen(false)}
            className={gameButtonClass({ tone: "neutral", size: "md" })}
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
