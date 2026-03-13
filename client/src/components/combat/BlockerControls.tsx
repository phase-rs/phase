import { gameButtonClass } from "../ui/buttonStyles.ts";

interface BlockerControlsProps {
  onConfirm: () => void;
  assignmentCount: number;
}

export function BlockerControls({
  onConfirm,
  assignmentCount,
}: BlockerControlsProps) {
  return (
    <div className="fixed inset-x-0 bottom-24 z-30 flex justify-center px-3">
      <div className="flex w-full max-w-[min(26rem,calc(100vw-1.25rem))] flex-col gap-2 rounded-[20px] border border-white/10 bg-[#0b1020]/88 p-2 shadow-[0_20px_48px_rgba(0,0,0,0.44)] backdrop-blur-md sm:w-auto sm:max-w-none">
      <button
        onClick={onConfirm}
        className={gameButtonClass({ tone: "emerald", size: "md", className: "w-full sm:w-auto sm:min-w-[10.5rem]" })}
      >
        Confirm Blockers ({assignmentCount})
      </button>
      </div>
    </div>
  );
}
