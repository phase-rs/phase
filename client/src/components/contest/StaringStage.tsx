import { useEffect, useState } from "react";
import type { CSSProperties } from "react";

import type { ContestState } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useContestStore } from "../../stores/contestStore.ts";
import {
  STARE_FINALE_MS,
  stareContestPose,
  stareElapsedMs,
  stareFinale,
  stareStrain,
} from "./staring.ts";
import type { StarePose } from "./staring.ts";

/** Wall-clock cadence of the strain redraw. Each step is too small to see. */
const STRAIN_REDRAW_MS = 100;

/** The opening of the eye, and the two lids that close over it. Each lid's edge is one
 *  side of the opening, so a lid at rest shows the whole eye and sliding it in covers it. */
const EYE_OPENING = "M30 140C110 30 370 30 450 140C370 240 110 240 30 140Z";
const UPPER_LID = "M0-160H480V140H450C370 30 110 30 30 140H0Z";
const LOWER_LID = "M0 320H480V140H450C370 240 110 240 30 140H0Z";

/** Burst capillaries, rooted at the rim and creeping in toward the iris as strain builds. */
const VEINS = [
  "M46 138C80 132 112 146 140 134",
  "M62 112C92 118 118 108 148 118",
  "M74 170C100 160 124 170 150 156",
  "M118 80C132 92 146 90 164 104",
  "M124 196C138 184 152 186 170 172",
  "M434 140C402 132 372 148 340 136",
  "M416 110C388 118 362 106 332 118",
  "M404 172C380 160 356 172 330 158",
  "M360 80C346 94 332 90 316 104",
  "M356 198C342 184 328 188 310 172",
] as const;

/** A tear rolling off the lower lid, and the strain at which it starts to run. */
const TEARS = [
  { x: 150, delayS: 0, strain: 0.25 },
  { x: 330, delayS: 1.1, strain: 0.45 },
  { x: 205, delayS: 0.6, strain: 0.65 },
  { x: 285, delayS: 1.7, strain: 0.85 },
] as const;

/** The tears a blink wrings out of the eye. */
const SQUEEZED_TEAR_XS = [185, 240, 300] as const;

const TEAR_DROP = "M0-10C5-2 8 2 8 7A8 8 0 0 1-8 7C-8 2-5-2 0-10Z";

/**
 * Time since GO, eased into strain. Holds its last value when the contest ends, so
 * the closing blink plays from exactly the state the eye reached.
 */
function useStareStrain(contest: ContestState | null): number {
  const [strain, setStrain] = useState(0);
  const phase = contest?.phase;
  const clockMs = contest?.clock_ms ?? 0;
  const goAtMs = contest?.go_at_ms;
  const pausedSinceMs = contest?.paused_since_ms;
  useEffect(() => {
    if (phase === "Pending" || phase === "Preparing") {
      setStrain(0);
      return;
    }
    if (phase !== "Active") return;
    const snapshotAt = performance.now();
    const timing = { clock_ms: clockMs, go_at_ms: goAtMs, paused_since_ms: pausedSinceMs };
    const redraw = () => setStrain(stareStrain(stareElapsedMs(timing, performance.now() - snapshotAt)));
    redraw();
    if (pausedSinceMs != null) return;
    const id = window.setInterval(redraw, STRAIN_REDRAW_MS);
    return () => window.clearInterval(id);
  }, [phase, clockMs, goAtMs, pausedSinceMs]);
  return strain;
}

/** The eye itself. Decorative: the contest and its result are always named in text.
 *  Pose and strain live here; every movement they imply is in `index.css`. */
function StaringEye({ pose, strain }: { pose: StarePose; strain: number }) {
  const tears = TEARS.filter((tear) => strain >= tear.strain);
  const squeezed = pose === "EyeBlinks" || pose === "BothBlink" ? SQUEEZED_TEAR_XS : [];
  return (
    <svg
      viewBox="24 46 432 186"
      className="stare-eye size-full overflow-visible"
      aria-hidden="true"
      focusable="false"
    >
      <defs>
        <clipPath id="stare-opening">
          <path d={EYE_OPENING} />
        </clipPath>
        <radialGradient id="stare-aura">
          <stop offset="55%" stopColor="#dc2626" />
          <stop offset="100%" stopColor="#dc2626" stopOpacity="0" />
        </radialGradient>
        <radialGradient id="stare-sclera">
          <stop offset="0%" stopColor="#f8f4ea" />
          <stop offset="70%" stopColor="#e9dfcf" />
          <stop offset="100%" stopColor="#a8927f" />
        </radialGradient>
        <radialGradient id="stare-bloodshot">
          <stop offset="35%" stopColor="#dc2626" stopOpacity="0" />
          <stop offset="100%" stopColor="#b91c1c" />
        </radialGradient>
        <radialGradient id="stare-iris">
          <stop offset="30%" stopColor="#fbbf24" />
          <stop offset="62%" stopColor="#b45309" />
          <stop offset="100%" stopColor="#451a03" />
        </radialGradient>
        <linearGradient id="stare-lid" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#1e1b2e" />
          <stop offset="100%" stopColor="#4c3a52" />
        </linearGradient>
        <linearGradient id="stare-water" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#e0f2fe" stopOpacity="0.8" />
          <stop offset="100%" stopColor="#7dd3fc" stopOpacity="0.45" />
        </linearGradient>
      </defs>

      <g className="stare-tremble">
        <ellipse className="stare-aura" cx="240" cy="136" rx="300" ry="170" fill="url(#stare-aura)" />
        <g clipPath="url(#stare-opening)">
          <rect x="0" y="40" width="480" height="200" fill="url(#stare-sclera)" />
          <g className="stare-veins">
            {VEINS.map((vein) => (
              <path key={vein} d={vein} pathLength={1} />
            ))}
          </g>
          <rect className="stare-bloodshot" x="0" y="40" width="480" height="200" fill="url(#stare-bloodshot)" />
          <g className="stare-iris">
            <circle cx="240" cy="136" r="66" fill="url(#stare-iris)" />
            <circle cx="240" cy="136" r="66" fill="none" stroke="#1c0a02" strokeWidth="5" />
            <circle className="stare-pupil" cx="240" cy="136" r="30" fill="#050505" />
            <circle cx="218" cy="112" r="11" fill="#fff" fillOpacity="0.9" />
            <circle cx="264" cy="156" r="5" fill="#fff" fillOpacity="0.5" />
          </g>
          <ellipse className="stare-glaze" cx="240" cy="120" rx="170" ry="62" />
          <path
            className="stare-water"
            d="M-120 150Q-60 142 0 150T120 150T240 150T360 150T480 150T600 150V320H-120Z"
            fill="url(#stare-water)"
          />
          <g className="stare-flutter">
            <path className="stare-lid-upper" d={UPPER_LID} fill="url(#stare-lid)" />
          </g>
          <path className="stare-lid-lower" d={LOWER_LID} fill="url(#stare-lid)" />
        </g>
        <path className="stare-rim" d={EYE_OPENING} />
      </g>

      {tears.map((tear) => (
        <path
          key={tear.x}
          data-testid="stare-tear"
          className="stare-tear"
          d={TEAR_DROP}
          style={{ "--stare-tear-x": `${tear.x}px`, animationDelay: `${tear.delayS}s` } as CSSProperties}
        />
      ))}
      {squeezed.map((x, index) => (
        <path
          key={x}
          data-testid="stare-squeezed-tear"
          className="stare-tear stare-tear-squeezed"
          d={TEAR_DROP}
          style={{ "--stare-tear-x": `${x}px`, animationDelay: `${0.18 + index * 0.14}s` } as CSSProperties}
        />
      ))}
    </svg>
  );
}

/**
 * The large eye of a staring contest (Eye to Eye). It opens at GO, strains and wells
 * up for as long as the contest runs, and blinks the moment the engine settles it
 * against the other side. If the viewer is the one who lost, it is the viewer's own
 * eyelids that close over the screen while the eye stays open. Mounted alongside the
 * contest panel rather than inside it because this outlives the contest: the engine
 * has already moved on by the time the blink plays, so it is non-interactive and never
 * holds the game up. Other contest kinds, and a cancelled contest, have no stage.
 */
export function StaringStage({ contest }: { contest: ContestState | null }) {
  const staring = contest?.kind === "Staring" ? contest : null;
  const strain = useStareStrain(staring);
  const me = usePlayerId();
  const announcement = useContestStore((s) => s.announcement);
  const announcementId = announcement?.id;
  const finale = announcement ? stareFinale(announcement, me) : null;

  const [playedFinale, setPlayedFinale] = useState<number | null>(null);
  useEffect(() => {
    if (finale == null || announcementId == null) return;
    const timer = window.setTimeout(() => setPlayedFinale(announcementId), STARE_FINALE_MS);
    return () => window.clearTimeout(timer);
  }, [finale, announcementId]);

  const pose: StarePose | null = staring
    ? stareContestPose(staring)
    : playedFinale === announcementId
      ? null
      : finale;
  if (pose == null) return null;

  const viewerBlinks = pose === "ViewerBlinks" || pose === "BothBlink";
  return (
    <div
      data-testid="staring-stage"
      data-pose={pose}
      aria-hidden="true"
      className="stare-stage pointer-events-none fixed inset-0 z-50 overflow-hidden"
      style={{ "--stare-strain": strain, "--stare-finale": `${STARE_FINALE_MS}ms` } as CSSProperties}
    >
      <div className="absolute inset-x-0 top-0 h-[44dvh] p-2 [@media(max-height:600px)]:h-full [@media(max-height:600px)]:w-1/2">
        <StaringEye pose={pose} strain={strain} />
      </div>
      {viewerBlinks && (
        <div data-testid="stare-viewer-blink">
          <div className="stare-viewer-lid stare-viewer-lid-upper" />
          <div className="stare-viewer-lid stare-viewer-lid-lower" />
        </div>
      )}
    </div>
  );
}
