interface BlockerControlsProps {
  onConfirm: () => void;
  assignmentCount: number;
}

export function BlockerControls({
  onConfirm,
  assignmentCount,
}: BlockerControlsProps) {
  return (
    <div className="fixed inset-x-0 bottom-24 z-30 flex justify-center gap-3">
      <button
        onClick={onConfirm}
        className="rounded-lg bg-green-600 px-4 py-2 text-sm font-semibold text-white shadow-lg transition hover:bg-green-500"
      >
        Confirm Blockers ({assignmentCount})
      </button>
    </div>
  );
}
