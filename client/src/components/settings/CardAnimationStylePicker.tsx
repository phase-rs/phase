import { useReducedMotion } from "framer-motion";
import { useEffect, useRef, useState, type FocusEvent, type KeyboardEvent, type PointerEvent } from "react";
import { useTranslation } from "react-i18next";

import type { CardAnimationStyle } from "../../animation/types.ts";
import {
  CARD_ANIMATION_PREVIEW_CLIPS,
  CARD_ANIMATION_PREVIEW_MOMENTS,
  momentIndexAt,
} from "./cardAnimationPreview.ts";

const STYLES: CardAnimationStyle[] = ["webgl", "classic"];

/** How far the two clips may drift apart, in seconds, before they are re-synced. */
const MAX_DRIFT_S = 0.12;

interface CardAnimationStylePickerProps {
  value: CardAnimationStyle;
  onChange: (style: CardAnimationStyle) => void;
}

/**
 * Chooses between the New and Classic card animations by showing both.
 *
 * Hovering (or focusing) the tiles plays the same game moment in each style,
 * side by side and in lockstep, looping for as long as the pointer stays.
 * Leaving pauses on the frame where the styles differ most. Clicking only
 * selects. Touch screens have no hover, so a tap toggles the preview. With
 * reduced motion requested, nothing plays: the posters and the selection stay.
 */
export function CardAnimationStylePicker({ value, onChange }: CardAnimationStylePickerProps) {
  const { t } = useTranslation("settings");
  const videos = useRef<Partial<Record<CardAnimationStyle, HTMLVideoElement>>>({});
  const previewingRef = useRef(false);
  const [previewing, setPreviewing] = useState(false);
  const [momentIndex, setMomentIndex] = useState(0);
  const reduceMotion = useReducedMotion();

  const forEachVideo = (fn: (video: HTMLVideoElement) => void) => {
    for (const style of STYLES) {
      const video = videos.current[style];
      if (video) fn(video);
    }
  };

  const startPreview = () => {
    if (previewingRef.current || reduceMotion) return;
    previewingRef.current = true;
    setPreviewing(true);
    forEachVideo((video) => {
      video.currentTime = CARD_ANIMATION_PREVIEW_MOMENTS[momentIndex].start;
      // A rejected play() (autoplay policy, interrupted load) leaves the still frame showing.
      video.play().catch(() => {});
    });
  };

  const stopPreview = () => {
    if (!previewingRef.current) return;
    previewingRef.current = false;
    setPreviewing(false);
    forEachVideo((video) => {
      video.pause();
      video.currentTime = CARD_ANIMATION_PREVIEW_MOMENTS[momentIndex].peak;
    });
  };

  useEffect(() => {
    if (reduceMotion) stopPreview();
    // stopPreview only reads refs and the current moment; re-run on the preference alone.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reduceMotion]);

  const handleTimeUpdate = () => {
    const lead = videos.current[STYLES[0]];
    if (!previewingRef.current || !lead) return;
    const index = momentIndexAt(lead.currentTime);
    if (index !== momentIndex) setMomentIndex(index);
    for (const style of STYLES.slice(1)) {
      const follower = videos.current[style];
      if (follower && Math.abs(follower.currentTime - lead.currentTime) > MAX_DRIFT_S) {
        follower.currentTime = lead.currentTime;
      }
    }
  };

  const handlePointerEnter = (e: PointerEvent) => {
    if (e.pointerType !== "touch") startPreview();
  };
  const handlePointerLeave = (e: PointerEvent) => {
    if (e.pointerType !== "touch") stopPreview();
  };
  const handlePointerDown = (e: PointerEvent) => {
    if (e.pointerType !== "touch") return;
    if (previewingRef.current) stopPreview();
    else startPreview();
  };
  const handleBlur = (e: FocusEvent<HTMLDivElement>) => {
    if (!e.currentTarget.contains(e.relatedTarget)) stopPreview();
  };
  const handleFocus = (e: FocusEvent<HTMLDivElement>) => {
    if (e.target.matches(":focus-visible")) startPreview();
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
    if (step === 0) return;
    e.preventDefault();
    const next = STYLES[(index + step + STYLES.length) % STYLES.length];
    onChange(next);
    e.currentTarget.parentElement?.querySelector<HTMLButtonElement>(`[data-style="${next}"]`)?.focus();
    startPreview();
  };

  return (
    <div className="max-w-sm">
      <div
        role="radiogroup"
        aria-label={t("visual.cardAnimationStyle")}
        className="grid grid-cols-2 gap-3"
        onPointerEnter={handlePointerEnter}
        onPointerLeave={handlePointerLeave}
        onPointerDown={handlePointerDown}
        onFocus={handleFocus}
        onBlur={handleBlur}
      >
        {STYLES.map((style, index) => {
          const selected = style === value;
          const clip = CARD_ANIMATION_PREVIEW_CLIPS[style];
          return (
            <button
              key={style}
              type="button"
              role="radio"
              aria-checked={selected}
              tabIndex={selected ? 0 : -1}
              data-style={style}
              onClick={() => onChange(style)}
              onKeyDown={(e) => handleKeyDown(e, index)}
              className={`flex min-w-0 flex-col gap-2 rounded-[16px] border-2 p-2 text-left transition-colors ${
                selected
                  ? "border-sky-500/80 bg-sky-500/10"
                  : "border-white/10 bg-black/18 hover:border-sky-400/40"
              }`}
            >
              <span className="relative block aspect-[390/844] w-full overflow-hidden rounded-[10px] bg-slate-950" aria-hidden>
                <video
                  ref={(node) => {
                    if (node) videos.current[style] = node;
                    else delete videos.current[style];
                  }}
                  src={clip.video}
                  poster={clip.poster}
                  muted
                  loop
                  playsInline
                  preload="metadata"
                  disablePictureInPicture
                  disableRemotePlayback
                  tabIndex={-1}
                  onTimeUpdate={style === STYLES[0] ? handleTimeUpdate : undefined}
                  className="absolute inset-0 h-full w-full object-cover"
                />
                <span
                  className={`pointer-events-none absolute bottom-2 left-1/2 -translate-x-1/2 whitespace-nowrap rounded-full bg-slate-950/80 px-2 py-1 text-[0.6rem] font-semibold uppercase tracking-[0.12em] text-white transition-opacity ${
                    previewing || reduceMotion ? "opacity-0" : "opacity-100"
                  }`}
                >
                  <span className="pointer-coarse:hidden">{t("visual.cardAnimationPreviewHintHover")}</span>
                  <span className="hidden pointer-coarse:inline">{t("visual.cardAnimationPreviewHintTap")}</span>
                </span>
              </span>
              <span className="flex items-center justify-between gap-2 px-1">
                <span className={`text-sm font-semibold ${selected ? "text-white" : "text-slate-300"}`}>
                  {t(`visual.cardAnimationStyleOptions.${style}`)}
                </span>
                <span
                  className={`flex h-4 w-4 flex-none items-center justify-center rounded-full border-2 ${
                    selected ? "border-sky-500 bg-sky-500" : "border-slate-500"
                  }`}
                  aria-hidden
                >
                  {selected && <span className="h-1.5 w-1.5 rounded-full bg-white" />}
                </span>
              </span>
            </button>
          );
        })}
      </div>
      <p className="mt-2 text-center text-xs text-slate-500" aria-hidden>
        {CARD_ANIMATION_PREVIEW_MOMENTS[momentIndex].spell}
      </p>
    </div>
  );
}
