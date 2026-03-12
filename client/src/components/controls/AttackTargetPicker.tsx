import { useState } from "react";

import type { AttackTarget, ObjectId, PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";

interface AttackTargetPickerProps {
  validTargets: AttackTarget[];
  selectedAttackers: ObjectId[];
  onConfirm: (attacks: [ObjectId, AttackTarget][]) => void;
  onCancel: () => void;
}

/**
 * Per-creature attack target selection for multiplayer games.
 *
 * Two modes:
 * - "all" (default): pick one target, all attackers go there
 * - "split": assign each attacker to a different target
 */
export function AttackTargetPicker({
  validTargets,
  selectedAttackers,
  onConfirm,
  onCancel,
}: AttackTargetPickerProps) {
  const [mode, setMode] = useState<"all" | "split">("all");
  const [perCreatureTargets, setPerCreatureTargets] = useState<Map<ObjectId, AttackTarget>>(
    () => new Map(),
  );

  const gameState = useGameStore((s) => s.gameState);
  const myId = usePlayerId();

  const teamBased = gameState?.format_config?.team_based ?? false;

  function getTargetLabel(target: AttackTarget): string {
    if (target.type === "Player") {
      return getPlayerLabel(target.data, myId, teamBased);
    }
    // Planeswalker: show name from game objects
    const obj = gameState?.objects[target.data];
    return obj?.name ?? `Planeswalker #${target.data}`;
  }

  function handleAttackAll(target: AttackTarget) {
    onConfirm(selectedAttackers.map((id) => [id, target]));
  }

  function handleSplitConfirm() {
    const attacks: [ObjectId, AttackTarget][] = selectedAttackers.map((id) => {
      const target = perCreatureTargets.get(id) ?? validTargets[0];
      return [id, target];
    });
    onConfirm(attacks);
  }

  function setCreatureTarget(creatureId: ObjectId, target: AttackTarget) {
    setPerCreatureTargets((prev) => {
      const next = new Map(prev);
      next.set(creatureId, target);
      return next;
    });
  }

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/60">
      <div className="w-[420px] max-h-[80vh] overflow-y-auto rounded-xl border border-gray-600 bg-gray-900/95 p-5 shadow-2xl backdrop-blur-sm">
        <h3 className="mb-4 text-center text-lg font-bold text-gray-100">
          Choose Attack Target
        </h3>

        {/* Mode toggle */}
        <div className="mb-4 flex justify-center gap-2">
          <button
            onClick={() => setMode("all")}
            className={gameButtonClass({
              tone: mode === "all" ? "blue" : "slate",
              size: "sm",
            })}
          >
            Attack All
          </button>
          <button
            onClick={() => setMode("split")}
            className={gameButtonClass({
              tone: mode === "split" ? "blue" : "slate",
              size: "sm",
            })}
          >
            Split Attacks
          </button>
        </div>

        {mode === "all" ? (
          /* Attack All mode: one button per target */
          <div className="flex flex-col gap-2">
            {validTargets.map((target) => (
              <button
                key={attackTargetKey(target)}
                onClick={() => handleAttackAll(target)}
                className={gameButtonClass({ tone: "red", size: "md" })}
              >
                Attack {getTargetLabel(target)} with {selectedAttackers.length} {selectedAttackers.length === 1 ? "creature" : "creatures"}
              </button>
            ))}
          </div>
        ) : (
          /* Split mode: per-creature assignment */
          <div className="flex flex-col gap-3">
            {selectedAttackers.map((creatureId) => {
              const obj = gameState?.objects[creatureId];
              const currentTarget = perCreatureTargets.get(creatureId) ?? validTargets[0];
              return (
                <div key={creatureId} className="flex items-center gap-2">
                  <span className="min-w-[120px] truncate text-sm text-gray-200">
                    {obj?.name ?? `Creature #${creatureId}`}
                  </span>
                  <div className="flex gap-1">
                    {validTargets.map((target) => (
                      <button
                        key={attackTargetKey(target)}
                        onClick={() => setCreatureTarget(creatureId, target)}
                        className={gameButtonClass({
                          tone: attackTargetsEqual(currentTarget, target) ? "red" : "slate",
                          size: "xs",
                        })}
                      >
                        {getTargetLabel(target)}
                      </button>
                    ))}
                  </div>
                </div>
              );
            })}
            <button
              onClick={handleSplitConfirm}
              className={gameButtonClass({ tone: "emerald", size: "md" })}
            >
              Confirm Split Attacks
            </button>
          </div>
        )}

        <button
          onClick={onCancel}
          className={`mt-3 w-full ${gameButtonClass({ tone: "slate", size: "sm" })}`}
        >
          Cancel
        </button>
      </div>
    </div>
  );
}

/** Stable key for an AttackTarget. */
function attackTargetKey(target: AttackTarget): string {
  return `${target.type}-${target.data}`;
}

/** Check equality of two AttackTargets. */
function attackTargetsEqual(a: AttackTarget, b: AttackTarget): boolean {
  return a.type === b.type && a.data === b.data;
}

/** Human-readable label for a player. */
function getPlayerLabel(playerId: PlayerId, myId: PlayerId, teamBased: boolean): string {
  if (playerId === myId) return "You";
  if (teamBased && Math.floor(playerId / 2) === Math.floor(myId / 2)) return "Ally";
  return `Player ${playerId + 1}`;
}
