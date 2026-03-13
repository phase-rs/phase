import { gameButtonClass } from "../ui/buttonStyles.ts";

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
    <div className="fixed inset-x-0 bottom-24 z-30 flex justify-center px-3">
      <div className="flex w-full max-w-[min(26rem,calc(100vw-1.25rem))] flex-col gap-2 rounded-[20px] border border-white/10 bg-[#0b1020]/88 p-2 shadow-[0_20px_48px_rgba(0,0,0,0.44)] backdrop-blur-md sm:w-auto sm:max-w-none sm:flex-row">
      <button
        onClick={onAttackAll}
        className={gameButtonClass({ tone: "amber", size: "md", className: "w-full sm:w-auto" })}
      >
        Attack All
      </button>
      <button
        onClick={onSkip}
        className={gameButtonClass({ tone: "slate", size: "md", className: "w-full sm:w-auto sm:min-w-[10.5rem]" })}
      >
        Skip
      </button>
      <button
        onClick={onConfirm}
        className={gameButtonClass({ tone: "emerald", size: "md", className: "w-full sm:w-auto sm:min-w-[10.5rem]" })}
      >
        Confirm Attackers ({attackerCount})
      </button>
      </div>
    </div>
  );
}
