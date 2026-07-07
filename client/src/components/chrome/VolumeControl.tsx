import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { audioManager } from "../../audio/AudioManager.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";

function SpeakerIcon({ className }: { className?: string }) {
  return (
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className={className ?? "w-5 h-5"}>
      <path d="M10.5 3.75a.75.75 0 0 0-1.264-.546L5.203 7H2.667a.75.75 0 0 0-.7.48A6.985 6.985 0 0 0 1.5 10c0 .887.165 1.737.468 2.52.111.29.39.48.7.48h2.535l4.033 3.796A.75.75 0 0 0 10.5 16.25V3.75ZM13.38 7.879a.75.75 0 0 1 1.06 0A4.983 4.983 0 0 1 15.75 11a4.983 4.983 0 0 1-1.31 3.121.75.75 0 1 1-1.06-1.06A3.483 3.483 0 0 0 14.25 11c0-.92-.355-1.758-.94-2.381a.75.75 0 0 1 .07-1.06Z" />
    </svg>
  );
}

function SpeakerMutedIcon({ className }: { className?: string }) {
  return (
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className={className ?? "w-5 h-5"}>
      <path d="M9.547 3.062A.75.75 0 0 1 10.5 3.75v12.5a.75.75 0 0 1-1.264.546L5.203 13H2.667a.75.75 0 0 1-.7-.48A6.985 6.985 0 0 1 1.5 10c0-.887.165-1.737.468-2.52a.75.75 0 0 1 .7-.48h2.535l4.033-3.796a.75.75 0 0 1 .811-.142ZM13.28 7.22a.75.75 0 1 0-1.06 1.06L13.94 10l-1.72 1.72a.75.75 0 0 0 1.06 1.06L15 11.06l1.72 1.72a.75.75 0 1 0 1.06-1.06L16.06 10l1.72-1.72a.75.75 0 0 0-1.06-1.06L15 8.94l-1.72-1.72Z" />
    </svg>
  );
}

function SpeakerLowIcon({ className }: { className?: string }) {
  return (
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className={className ?? "w-5 h-5"}>
      <path d="M10.5 3.75a.75.75 0 0 0-1.264-.546L5.203 7H2.667a.75.75 0 0 0-.7.48A6.985 6.985 0 0 0 1.5 10c0 .887.165 1.737.468 2.52.111.29.39.48.7.48h2.535l4.033 3.796A.75.75 0 0 0 10.5 16.25V3.75Z" />
    </svg>
  );
}

interface VolumeControlProps {
  /** "chrome" = ScreenChrome style (menu pages), "game" = GameMenu style (in-game) */
  variant: "chrome" | "game";
}

export function VolumeControl({ variant }: VolumeControlProps) {
  const { t } = useTranslation();
  const masterVolume = usePreferencesStore((s) => s.masterVolume);
  const masterMuted = usePreferencesStore((s) => s.masterMuted);
  const setMasterVolume = usePreferencesStore((s) => s.setMasterVolume);
  const setMasterMuted = usePreferencesStore((s) => s.setMasterMuted);
  const [expanded, setExpanded] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const hideTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const handleToggleMute = useCallback(() => {
    const next = !masterMuted;
    setMasterMuted(next);
    if (next) {
      // Fully tear down AudioContext so iOS doesn't leave it in a broken suspended state
      audioManager.dispose();
    } else {
      // Rebuild from scratch — resume() alone can't recover from iOS "interrupted" state
      audioManager.restart();
    }
  }, [masterMuted, setMasterMuted]);

  const handleVolumeChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const vol = Number(e.target.value);
      setMasterVolume(vol);
      if (masterMuted && vol > 0) {
        setMasterMuted(false);
        audioManager.ensurePlayback();
      }
    },
    [masterMuted, setMasterVolume, setMasterMuted],
  );

  const scheduleHide = useCallback(() => {
    hideTimerRef.current = setTimeout(() => setExpanded(false), 300);
  }, []);

  const cancelHide = useCallback(() => {
    if (hideTimerRef.current) {
      clearTimeout(hideTimerRef.current);
      hideTimerRef.current = null;
    }
  }, []);

  const handleMouseEnter = useCallback(() => {
    cancelHide();
    setExpanded(true);
  }, [cancelHide]);

  const handleMouseLeave = useCallback(() => {
    scheduleHide();
  }, [scheduleHide]);

  // Close on outside click
  useEffect(() => {
    if (!expanded) return;
    function handleClick(e: MouseEvent) {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setExpanded(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [expanded]);

  useEffect(() => {
    return () => {
      if (hideTimerRef.current) clearTimeout(hideTimerRef.current);
    };
  }, []);

  const effectiveVolume = masterMuted ? 0 : masterVolume;
  const Icon = masterMuted || effectiveVolume === 0
    ? SpeakerMutedIcon
    : effectiveVolume < 50
      ? SpeakerLowIcon
      : SpeakerIcon;

  const sliderValue = masterMuted ? 0 : masterVolume;
  const sliderLabel = `${masterMuted ? 0 : masterVolume}%`;

  if (variant === "game") {
    return (
      <div
        ref={containerRef}
        className="flex h-7 items-center overflow-hidden rounded-md bg-white/6 transition-all duration-200"
        style={{ width: expanded ? 168 : 28 }}
        onMouseEnter={handleMouseEnter}
        onMouseLeave={handleMouseLeave}
      >
        <button
          onClick={handleToggleMute}
          className="flex h-7 w-7 shrink-0 items-center justify-center text-gray-400 transition-colors hover:text-gray-200"
          aria-label={masterMuted ? t("volume.unmute") : t("volume.mute")}
          title={masterMuted ? t("volume.unmute") : t("volume.mute")}
        >
          <Icon className="h-4 w-4" />
        </button>
        <div
          className="flex items-center gap-2 pr-3 transition-opacity duration-200"
          style={{ opacity: expanded ? 1 : 0 }}
        >
          <input
            type="range"
            min={0}
            max={100}
            value={sliderValue}
            onChange={handleVolumeChange}
            className="w-20 accent-cyan-500"
            aria-label={t("volume.volume")}
            tabIndex={expanded ? 0 : -1}
          />
          <span className="w-8 text-right text-[10px] text-gray-400">{sliderLabel}</span>
        </div>
      </div>
    );
  }

  // variant === "chrome" — icon on right, slider expands leftward
  // (pinned to upper-right corner, so the icon stays at the screen edge)
  // flex-row-reverse keeps the icon (first in DOM) visually on the right,
  // so it's always visible even when collapsed with overflow-hidden.
  return (
    <div
      ref={containerRef}
      className="flex flex-row-reverse min-h-9 items-center overflow-hidden rounded-[8px] border border-white/12 bg-slate-950/82 transition-all duration-200"
      style={{ width: expanded ? 180 : 36 }}
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
    >
      <button
        onClick={handleToggleMute}
        className="flex min-h-9 min-w-9 shrink-0 items-center justify-center text-white/46 transition-colors hover:text-white/72"
        aria-label={masterMuted ? t("volume.unmute") : t("volume.mute")}
        title={masterMuted ? t("volume.unmute") : t("volume.mute")}
      >
        <Icon />
      </button>
      <div
        className="flex items-center gap-2 pl-3 transition-opacity duration-200"
        style={{ opacity: expanded ? 1 : 0 }}
      >
        <span className="w-8 text-xs text-slate-400">{sliderLabel}</span>
        <input
          type="range"
          min={0}
          max={100}
          value={sliderValue}
          onChange={handleVolumeChange}
          className="w-24 accent-cyan-500"
          aria-label={t("volume.volume")}
          tabIndex={expanded ? 0 : -1}
        />
      </div>
    </div>
  );
}
