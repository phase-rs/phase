import { useTranslation } from "react-i18next";

import { useReplayStore } from "../../stores/replayStore";

const SPEED_OPTIONS = [0.5, 1, 2, 4, 8];

/**
 * Scrubber + playback transport for the Replay Viewer. Reads/drives
 * `useReplayStore` exclusively — it never touches `useGameStore` directly,
 * `replayStore.seek` is the only thing that mirrors a reconstructed state
 * into the shared game store the board renders from.
 */
export function ReplayControls() {
  const { t } = useTranslation("replay");

  const currentIndex = useReplayStore((s) => s.currentIndex);
  const totalActions = useReplayStore((s) => s.totalActions);
  const isPlaying = useReplayStore((s) => s.isPlaying);
  const playbackSpeed = useReplayStore((s) => s.playbackSpeed);
  const error = useReplayStore((s) => s.error);
  const seek = useReplayStore((s) => s.seek);
  const stepBackward = useReplayStore((s) => s.stepBackward);
  const stepForward = useReplayStore((s) => s.stepForward);
  const play = useReplayStore((s) => s.play);
  const pause = useReplayStore((s) => s.pause);
  const setSpeed = useReplayStore((s) => s.setSpeed);

  return (
    <div className="flex w-full flex-col gap-2 border-t border-white/10 bg-black/70 px-4 py-3 backdrop-blur">
      {error && (
        <p className="text-xs text-red-400" role="alert">
          {t("controls.seekError", { message: error })}
        </p>
      )}
      <input
        type="range"
        min={0}
        max={Math.max(totalActions, 0)}
        value={currentIndex}
        onChange={(e) => void seek(Number(e.target.value))}
        className="w-full accent-amber-400"
        aria-label={t("controls.actionOf", { current: currentIndex, total: totalActions })}
      />
      <div className="flex items-center justify-between gap-3 text-sm text-slate-200">
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => void stepBackward()}
            disabled={currentIndex <= 0}
            className="rounded bg-white/10 px-2 py-1 hover:bg-white/20 disabled:opacity-40"
            aria-label={t("controls.stepBack")}
          >
            «
          </button>
          <button
            type="button"
            onClick={() => (isPlaying ? pause() : play())}
            className="rounded bg-amber-500/90 px-3 py-1 font-semibold text-black hover:bg-amber-400"
          >
            {isPlaying ? t("controls.pause") : t("controls.play")}
          </button>
          <button
            type="button"
            onClick={() => void stepForward()}
            disabled={currentIndex >= totalActions}
            className="rounded bg-white/10 px-2 py-1 hover:bg-white/20 disabled:opacity-40"
            aria-label={t("controls.stepForward")}
          >
            »
          </button>
        </div>

        <span className="tabular-nums text-slate-400">
          {t("controls.actionOf", { current: currentIndex, total: totalActions })}
        </span>

        <label className="flex items-center gap-2 text-slate-400">
          {t("controls.speed")}
          <select
            value={playbackSpeed}
            onChange={(e) => setSpeed(Number(e.target.value))}
            className="rounded bg-white/10 px-1.5 py-1 text-slate-100"
          >
            {SPEED_OPTIONS.map((speed) => (
              <option key={speed} value={speed}>
                {t("controls.speedValue", { value: speed })}
              </option>
            ))}
          </select>
        </label>
      </div>
    </div>
  );
}
