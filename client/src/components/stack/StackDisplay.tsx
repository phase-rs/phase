import { AnimatePresence } from "framer-motion";

import { StackEntry } from "./StackEntry.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import type { StackEntry as StackEntryType } from "../../adapter/types.ts";

const EMPTY_STACK: StackEntryType[] = [];

export function StackDisplay() {
  const stack = useGameStore((s) => s.gameState?.stack ?? EMPTY_STACK);

  return (
    <div className="flex flex-col gap-1">
      <h3 className="text-xs font-semibold uppercase tracking-wider text-gray-400">
        Stack
      </h3>
      {stack.length === 0 ? (
        <p className="text-xs italic text-gray-600">Stack empty</p>
      ) : (
        <div className="flex flex-col gap-1">
          <AnimatePresence mode="popLayout">
            {stack.map((entry) => (
              <StackEntry key={entry.id} entry={entry} />
            ))}
          </AnimatePresence>
        </div>
      )}
    </div>
  );
}
