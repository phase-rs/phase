import { useGameStore } from "../../stores/gameStore.ts";

export function PassButton() {
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const stackSize = useGameStore((s) => s.gameState?.stack.length ?? 0);

  const hasPriority =
    waitingFor?.type === "Priority";

  if (!hasPriority) return null;

  const label = stackSize > 0 ? "Resolve" : "Done";

  return (
    <button
      onClick={() => dispatch({ type: "PassPriority" })}
      className="rounded-lg bg-blue-600 px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-blue-500 active:bg-blue-700"
    >
      {label}
    </button>
  );
}
