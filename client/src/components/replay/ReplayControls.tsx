import { useCallback, useEffect, useRef } from "react";

interface ReplayControlsProps {
  currentIndex: number;
  total: number;
  isPlaying: boolean;
  onPrev: () => void;
  onNext: () => void;
  onSeek: (index: number) => void;
  onPlayPause: () => void;
}

export function ReplayControls({
  currentIndex,
  total,
  isPlaying,
  onPrev,
  onNext,
  onSeek,
  onPlayPause,
}: ReplayControlsProps) {
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const clearTimer = useCallback(() => {
    if (intervalRef.current !== null) {
      clearInterval(intervalRef.current);
      intervalRef.current = null;
    }
  }, []);

  useEffect(() => {
    if (isPlaying) {
      intervalRef.current = setInterval(() => {
        onNext();
      }, 2000);
    } else {
      clearTimer();
    }
    return clearTimer;
  }, [isPlaying, onNext, clearTimer]);

  const btnBase =
    "px-3 py-1.5 rounded text-sm font-medium transition-colors disabled:opacity-40 disabled:cursor-not-allowed";

  return (
    <div className="flex flex-col gap-2 bg-[#1a1a1a] border border-[#333] rounded-lg p-3">
      {/* Scrubber */}
      <div className="flex items-center gap-2 text-xs text-[#aaa]">
        <span className="w-16 text-right">Turn {currentIndex + 1}</span>
        <input
          type="range"
          min={0}
          max={Math.max(0, total - 1)}
          value={currentIndex}
          onChange={(e) => onSeek(Number(e.target.value))}
          className="flex-1 accent-[#8b5cf6] cursor-pointer"
        />
        <span className="w-16">of {total}</span>
      </div>

      {/* Buttons */}
      <div className="flex items-center justify-center gap-2">
        <button
          className={`${btnBase} bg-[#2a2a2a] hover:bg-[#3a3a3a] text-white`}
          onClick={onPrev}
          disabled={currentIndex === 0}
          aria-label="Previous turn"
        >
          ← Prev
        </button>

        <button
          className={`${btnBase} ${isPlaying ? "bg-[#7c3aed] hover:bg-[#6d28d9]" : "bg-[#4c1d95] hover:bg-[#5b21b6]"} text-white min-w-[80px]`}
          onClick={onPlayPause}
          aria-label={isPlaying ? "Pause replay" : "Play replay"}
        >
          {isPlaying ? "⏸ Pause" : "▶ Play"}
        </button>

        <button
          className={`${btnBase} bg-[#2a2a2a] hover:bg-[#3a3a3a] text-white`}
          onClick={onNext}
          disabled={currentIndex >= total - 1}
          aria-label="Next turn"
        >
          Next →
        </button>
      </div>
    </div>
  );
}
