import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion, useReducedMotion } from "framer-motion";

import type { AttackTarget, ObjectId, PlayerId } from "../../adapter/types.ts";
import { getSeatColor } from "../../hooks/useSeatColor.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { PeekTab } from "../modal/DialogShell.tsx";

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
  const [peeked, setPeeked] = useState(false);
  const [perCreatureTargets, setPerCreatureTargets] = useState<Map<ObjectId, AttackTarget>>(
    () => new Map(),
  );
  const shouldReduceMotion = useReducedMotion();

  const gameState = useGameStore((s) => s.gameState);
  const myId = usePlayerId();
  const hoverProps = useInspectHoverProps();
  const seatOrder = gameState?.seat_order;

  const teamBased = gameState?.format_config?.team_based ?? false;

  const sortedTargets = useMemo(() => {
    if (!seatOrder) return validTargets;
    return [...validTargets].sort((a, b) => {
      const aIdx = a.type === "Player" ? seatOrder.indexOf(a.data) : Infinity;
      const bIdx = b.type === "Player" ? seatOrder.indexOf(b.data) : Infinity;
      return aIdx - bIdx;
    });
  }, [validTargets, seatOrder]);

  function getTargetLabel(target: AttackTarget): string {
    if (target.type === "Player") {
      return getPlayerLabel(target.data, myId, teamBased);
    }
    const obj = gameState?.objects[target.data];
    return obj?.name ?? `Planeswalker #${target.data}`;
  }

  function getTargetSeatColor(target: AttackTarget): string | undefined {
    if (target.type === "Player") {
      return getSeatColor(target.data, seatOrder);
    }
    const obj = gameState?.objects[target.data];
    return obj ? getSeatColor(obj.controller, seatOrder) : undefined;
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

  function assignAllCreatures(target: AttackTarget) {
    setPerCreatureTargets(() => {
      const next = new Map<ObjectId, AttackTarget>();
      for (const id of selectedAttackers) {
        next.set(id, target);
      }
      return next;
    });
  }

  const slideTransform = peeked
    ? { x: "calc(100vw - 32px)" }
    : { x: 0 };

  return (
    <>
      <motion.div
        className="fixed inset-0 z-40 flex items-center justify-center bg-black/60"
        style={{ pointerEvents: peeked ? "none" : undefined }}
        animate={slideTransform}
        transition={
          shouldReduceMotion
            ? { duration: 0 }
            : { type: "spring", stiffness: 320, damping: 32 }
        }
      >
        <div className="relative w-[420px] max-h-[80vh] overflow-y-auto rounded-xl border border-gray-600 bg-gray-900/95 px-8 py-5 shadow-2xl backdrop-blur-sm">
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
              {sortedTargets.map((target) => {
                const color = getTargetSeatColor(target);
                return (
                  <button
                    key={attackTargetKey(target)}
                    onClick={() => handleAttackAll(target)}
                    className={gameButtonClass({ tone: "red", size: "md" })}
                  >
                    Attack <span className="mx-1 font-bold" style={color ? { color } : undefined}>{getTargetLabel(target)}</span> with {selectedAttackers.length} {selectedAttackers.length === 1 ? "creature" : "creatures"}
                  </button>
                );
              })}
            </div>
          ) : (
            /* Split mode: bulk-assign + per-creature dropdowns */
            <div className="flex flex-col gap-3">
              {/* Bulk-assign buttons */}
              <div className="flex flex-wrap justify-center gap-1.5">
                {sortedTargets.map((target) => {
                  const color = getTargetSeatColor(target);
                  return (
                    <button
                      key={`bulk-${attackTargetKey(target)}`}
                      onClick={() => assignAllCreatures(target)}
                      className={gameButtonClass({ tone: "slate", size: "xs" })}
                    >
                      All → <span className="ml-1 font-bold" style={color ? { color } : undefined}>{getTargetLabel(target)}</span>
                    </button>
                  );
                })}
              </div>

              {/* Per-creature assignment list */}
              <div className="flex max-h-[50vh] flex-col gap-1 overflow-y-auto">
                {selectedAttackers.map((creatureId) => {
                  const obj = gameState?.objects[creatureId];
                  const currentTarget = perCreatureTargets.get(creatureId) ?? validTargets[0];
                  return (
                    <div key={creatureId} className="flex items-center gap-2 rounded px-1 py-0.5 hover:bg-white/5">
                      <span className="min-w-0 flex-1 truncate text-sm text-gray-200" {...hoverProps(creatureId)}>
                        {obj?.name ?? `Creature #${creatureId}`}
                      </span>
                      <TargetDropdown
                        targets={sortedTargets}
                        currentTarget={currentTarget}
                        getLabel={getTargetLabel}
                        getColor={getTargetSeatColor}
                        onChange={(target) => setCreatureTarget(creatureId, target)}
                      />
                    </div>
                  );
                })}
              </div>

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
          <PeekTab onClick={() => setPeeked(true)} />
        </div>
      </motion.div>
      {peeked && <RestoreTab onClick={() => setPeeked(false)} />}
    </>
  );
}

/** Stable key for an AttackTarget. */
function attackTargetKey(target: AttackTarget): string {
  return `${target.type}-${target.data}`;
}

function RestoreTab({ onClick }: { onClick: () => void }) {
  return (
    <motion.button
      type="button"
      onClick={onClick}
      aria-label="Restore dialog"
      title="Restore attack target dialog"
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

interface TargetDropdownProps {
  targets: AttackTarget[];
  currentTarget: AttackTarget;
  getLabel: (target: AttackTarget) => string;
  getColor: (target: AttackTarget) => string | undefined;
  onChange: (target: AttackTarget) => void;
}

function TargetDropdown({ targets, currentTarget, getLabel, getColor, onChange }: TargetDropdownProps) {
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const currentColor = getColor(currentTarget);

  useEffect(() => {
    if (!open) return;
    function handleClick(e: MouseEvent) {
      if (
        !buttonRef.current?.contains(e.target as Node) &&
        !menuRef.current?.contains(e.target as Node)
      ) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open]);

  const [pos, setPos] = useState({ top: 0, right: 0 });
  useEffect(() => {
    if (!open || !buttonRef.current) return;
    const rect = buttonRef.current.getBoundingClientRect();
    setPos({
      top: rect.bottom + 4,
      right: window.innerWidth - rect.right,
    });
  }, [open]);

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="flex items-center gap-1.5 rounded border border-gray-600 bg-gray-800 px-2 py-1 text-sm text-gray-100 transition-colors hover:border-gray-400"
      >
        <span className="inline-block h-2.5 w-2.5 shrink-0 rounded-full" style={{ backgroundColor: currentColor ?? "#6b7280" }} />
        <span>{getLabel(currentTarget)}</span>
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3 w-3 text-gray-400">
          <path fillRule="evenodd" d="M4.22 6.22a.75.75 0 0 1 1.06 0L8 8.94l2.72-2.72a.75.75 0 1 1 1.06 1.06l-3.25 3.25a.75.75 0 0 1-1.06 0L4.22 7.28a.75.75 0 0 1 0-1.06Z" clipRule="evenodd" />
          </svg>
      </button>
      {open && createPortal(
        <AnimatePresence>
          <motion.div
            ref={menuRef}
            initial={{ opacity: 0, y: -4 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: -4 }}
            transition={{ duration: 0.12 }}
            className="fixed z-[100] min-w-[120px] overflow-hidden rounded-lg border border-gray-600 bg-gray-800 py-1 shadow-xl"
            style={{ top: pos.top, right: pos.right }}
          >
            {targets.map((target) => {
              const color = getColor(target);
              const isSelected = attackTargetKey(target) === attackTargetKey(currentTarget);
              return (
                <button
                  key={attackTargetKey(target)}
                  type="button"
                  onClick={() => { onChange(target); setOpen(false); }}
                  className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm transition-colors hover:bg-white/10 ${isSelected ? "text-white" : "text-gray-300"}`}
                >
                  <span className="inline-block h-2.5 w-2.5 shrink-0 rounded-full" style={{ backgroundColor: color ?? "#6b7280" }} />
                  <span>{getLabel(target)}</span>
                  {isSelected && (
                    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="ml-auto h-3.5 w-3.5 text-cyan-400">
                      <path fillRule="evenodd" d="M12.416 3.376a.75.75 0 0 1 .208 1.04l-5 7.5a.75.75 0 0 1-1.154.114l-3-3a.75.75 0 0 1 1.06-1.06l2.353 2.353 4.493-6.74a.75.75 0 0 1 1.04-.207Z" clipRule="evenodd" />
                    </svg>
                  )}
                </button>
              );
            })}
          </motion.div>
        </AnimatePresence>,
        document.body,
      )}
    </>
  );
}

function getPlayerLabel(playerId: PlayerId, myId: PlayerId, teamBased: boolean): string {
  if (playerId === myId) return "You";
  if (teamBased && Math.floor(playerId / 2) === Math.floor(myId / 2)) return "Ally";
  return getPlayerDisplayName(playerId, myId);
}
