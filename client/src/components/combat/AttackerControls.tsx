interface AttackerControlsProps {
  onAttackAll: () => void;
  onSkip: () => void;
  onConfirm: () => void;
  attackerCount: number;
}

export function AttackerControls({
  onAttackAll,
  onSkip,
  onConfirm,
  attackerCount,
}: AttackerControlsProps) {
  return (
    <div className="fixed inset-x-0 bottom-24 z-30 flex justify-center gap-3">
      <button
        onClick={onAttackAll}
        className="rounded-lg bg-amber-600 px-4 py-2 text-sm font-semibold text-white shadow-lg transition hover:bg-amber-500"
      >
        Attack All
      </button>
      <button
        onClick={onSkip}
        className="rounded-lg bg-gray-600 px-4 py-2 text-sm font-semibold text-white shadow-lg transition hover:bg-gray-500"
      >
        Skip
      </button>
      <button
        onClick={onConfirm}
        className="rounded-lg bg-green-600 px-4 py-2 text-sm font-semibold text-white shadow-lg transition hover:bg-green-500"
      >
        Confirm Attackers ({attackerCount})
      </button>
    </div>
  );
}
