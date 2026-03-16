import { useEffect, useRef, useState } from "react";

import {
  AI_DIFFICULTIES,
  getAiDifficultyLabel,
  type AIDifficulty,
} from "../../constants/ai";

interface AiDifficultyDropdownProps {
  difficulty: AIDifficulty;
  onChange: (difficulty: AIDifficulty) => void;
  align?: "left" | "right";
  className?: string;
  panelClassName?: string;
  compact?: boolean;
}

export function AiDifficultyDropdown({
  difficulty,
  onChange,
  align = "right",
  className,
  panelClassName,
  compact = false,
}: AiDifficultyDropdownProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;

    function handlePointerDown(event: MouseEvent) {
      if (rootRef.current && !rootRef.current.contains(event.target as Node)) {
        setOpen(false);
      }
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setOpen(false);
      }
    }

    document.addEventListener("mousedown", handlePointerDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handlePointerDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [open]);

  return (
    <div ref={rootRef} className={`relative ${className ?? ""}`}>
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={`AI difficulty: ${getAiDifficultyLabel(difficulty)}`}
        onClick={() => setOpen((current) => !current)}
        className={[
          "flex h-full min-h-11 items-center justify-center gap-2 px-3 text-sm font-medium text-white/88 transition-colors",
          "bg-white/[0.03] hover:bg-white/[0.08] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/30",
          compact ? "min-w-[6.25rem]" : "min-w-[7.75rem]",
        ].join(" ")}
      >
        <span className="truncate">{getAiDifficultyLabel(difficulty)}</span>
        <ChevronDownIcon open={open} />
      </button>

      {open && (
        <div
          role="menu"
          className={[
            "absolute top-full z-30 mt-2 min-w-[11rem] overflow-hidden rounded-[16px] border border-white/10 bg-[#0a0f1b]/96 p-1 shadow-[0_18px_50px_rgba(0,0,0,0.34)] backdrop-blur-xl",
            align === "right" ? "right-0" : "left-0",
            panelClassName,
          ].filter(Boolean).join(" ")}
        >
          {AI_DIFFICULTIES.map((item) => {
            const active = item.id === difficulty;

            return (
              <button
                key={item.id}
                type="button"
                role="menuitemradio"
                aria-checked={active}
                onClick={() => {
                  onChange(item.id);
                  setOpen(false);
                }}
                className={[
                  "flex w-full items-center justify-between rounded-[12px] px-3 py-2.5 text-left text-sm transition-colors",
                  active
                    ? "bg-indigo-400/14 text-indigo-100"
                    : "text-slate-200 hover:bg-white/[0.05]",
                ].join(" ")}
              >
                <span>{item.label}</span>
                {active && <CheckIcon />}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

function ChevronDownIcon({ open }: { open: boolean }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 20 20"
      className={`h-4 w-4 shrink-0 fill-current transition-transform ${open ? "rotate-180" : ""}`}
    >
      <path d="M5.47 7.97a.75.75 0 0 1 1.06 0L10 11.44l3.47-3.47a.75.75 0 1 1 1.06 1.06l-4 4a.75.75 0 0 1-1.06 0l-4-4a.75.75 0 0 1 0-1.06Z" />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 20 20" className="h-4 w-4 fill-current">
      <path
        fillRule="evenodd"
        d="M16.704 5.29a1 1 0 0 1 .006 1.414l-7.12 7.18a1 1 0 0 1-1.42.01l-3.88-3.88a1 1 0 1 1 1.414-1.414l3.17 3.17 6.41-6.464a1 1 0 0 1 1.42-.016Z"
        clipRule="evenodd"
      />
    </svg>
  );
}
