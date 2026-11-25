import {
  draftProgressPresentation,
  type DraftProgressFields,
  type DraftProgressPresentation,
} from "../../adapter/draft-adapter";
import { useDraftStore } from "../../stores/draftStore";

export function DraftProgress({ view: viewOverride }: { view?: DraftProgressFields | null } = {}) {
  const quickView = useDraftStore((s) => s.view);
  const view = viewOverride !== undefined ? viewOverride : quickView;

  if (!view) return null;

  // CR 903.13b: `pick_number` counts pick STEPS, not cards, so the bar's
  // denominator is the engine's published step count and never a card count —
  // a 14-card Commander pack is 7 steps, and 14 is a denominator the session
  // can never reach. A multi-set draft's boosters also differ from each other,
  // so the count is read per pack. This component computes nothing: it reads
  // one engine value instead of another. On a FIFO view the count is the seat's
  // own `round_completed_steps`, also a step count.
  const {
    current_pack_number,
    pack_pick_steps,
    pick_steps_per_pack,
    pack_set_codes,
    source,
    pack_count,
    pass_direction,
  } = view;
  const progress = draftProgressPresentation(view);
  const counterSteps = counterStepsFor(progress);
  const directionArrow = pass_direction === "Left" ? "←" : "→";
  // Engine-owned per-pack step counts. Views delivered before the field existed
  // fall back to the current booster's step count for every pack.
  const packSteps = (packIdx: number) => pack_pick_steps?.[packIdx] ?? pick_steps_per_pack;
  // A multi-set draft opens a different set each round, so the pack the player
  // is holding needs naming. A single-set draft names the same set every round,
  // where the label would be noise.
  const mixedSets = new Set(pack_set_codes ?? []).size > 1;
  const chaosCurrentPackCode = source?.type === "Set"
    && "Chaos" in source.data.layout
    ? source.data.layout.Chaos.current_pack_code
    : undefined;

  return (
    <div data-draft-progress className="flex items-center rounded-[16px] border border-hairline bg-white/[0.035] px-4 py-1.5 shadow-[inset_0_-1px_0_rgba(0,0,0,0.28)]">
      <div className="flex min-w-0 flex-1 flex-col gap-1.5 sm:flex-row sm:items-center sm:gap-2">
        {Array.from({ length: pack_count }, (_, packIdx) => {
          const isComplete = packIdx < current_pack_number;
          const isCurrent = packIdx === current_pack_number;
          const setCode = chaosCurrentPackCode !== undefined
            ? (isCurrent ? chaosCurrentPackCode ?? undefined : undefined)
            : (mixedSets ? pack_set_codes?.[packIdx] : undefined);

          return (
            <div key={packIdx} className="flex w-full min-w-0 items-center gap-1.5 sm:w-auto sm:flex-1">
              {packIdx > 0 && (
                <span className="shrink-0 pb-0.5 text-[10px] text-white/20">{directionArrow}</span>
              )}
              <span
                data-pack-number={packIdx + 1}
                className="shrink-0 font-display text-xs font-semibold tracking-[-0.02em] tabular-nums text-fg"
              >
                P{packIdx + 1}
              </span>
              {isCurrent ? (
                <CurrentPackProgress
                  progress={progress}
                  pickCount={packSteps(packIdx)}
                  packNumber={packIdx + 1}
                  setCode={setCode}
                />
              ) : (
                <PackSegment
                  pickCount={packSteps(packIdx)}
                  filledPicks={isComplete ? packSteps(packIdx) : 0}
                  packNumber={packIdx + 1}
                  isCurrent={false}
                  setCode={setCode}
                />
              )}
            </div>
          );
        })}
      </div>
      {counterSteps !== null && (
        <div data-draft-progress-counter className="shrink-0 text-xs tabular-nums text-white/45">
          <span className="font-semibold text-white">{counterSteps + 1}</span>
          <span>/{packSteps(current_pack_number)}</span>
        </div>
      )}
    </div>
  );
}

/**
 * The step the counter names, or `null` when the view publishes none: a FIFO
 * seat holding no pack is taking no step, and a spectator's seats each stand
 * at their own.
 */
function counterStepsFor(progress: DraftProgressPresentation): number | null {
  switch (progress.kind) {
    case "pod":
      return progress.completedSteps;
    case "seat":
      return progress.holdsHead ? progress.completedSteps : null;
    case "seats":
    case "unavailable":
      return null;
  }
}

/**
 * The booster being drafted, filled to the view's position: one row for a
 * player or legacy view, one row per seat on a FIFO spectator view, and no
 * fill when the view publishes no position.
 */
function CurrentPackProgress({
  progress,
  pickCount,
  packNumber,
  setCode,
}: {
  progress: DraftProgressPresentation;
  pickCount: number;
  packNumber: number;
  setCode?: string;
}) {
  switch (progress.kind) {
    case "pod":
    case "seat":
      return (
        <PackSegment
          pickCount={pickCount}
          filledPicks={progress.completedSteps}
          packNumber={packNumber}
          isCurrent
          setCode={setCode}
        />
      );
    case "seats":
      return (
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          {progress.seats.map((seat, row) => (
            <div key={seat.seatIndex} data-seat-progress={seat.seatIndex} className="flex min-w-0">
              <PackSegment
                pickCount={pickCount}
                filledPicks={seat.completedSteps}
                packNumber={packNumber}
                isCurrent
                setCode={row === 0 ? setCode : undefined}
              />
            </div>
          ))}
        </div>
      );
    case "unavailable":
      return (
        <PackSegment
          pickCount={pickCount}
          filledPicks={0}
          packNumber={packNumber}
          isCurrent
          setCode={setCode}
        />
      );
  }
}

function PackSegment({
  packNumber,
  pickCount,
  filledPicks,
  isCurrent,
  setCode,
}: {
  packNumber: number;
  pickCount: number;
  filledPicks: number;
  isCurrent: boolean;
  /** Only supplied when the draft mixes sets, where each pack needs naming. */
  setCode?: string;
}) {
  return (
    <div className="flex min-w-0 flex-1 flex-col gap-1">
      {setCode && (
        <span
          className={`truncate text-[9px] font-semibold uppercase tracking-wider ${
            isCurrent ? "text-amber-200/70" : "text-white/25"
          }`}
        >
          {setCode}
        </span>
      )}
      <div className="flex min-w-0 gap-px">
        {Array.from({ length: pickCount }, (_, i) => {
          const filled = i < filledPicks;
          const isLatest = isCurrent && i === filledPicks - 1;

          let bg: string;
          if (filled) {
            bg = isLatest
              ? "bg-amber-400/90"
              : "bg-amber-400/50";
          } else if (isCurrent) {
            bg = "bg-white/8";
          } else {
            bg = "bg-white/4";
          }

          return (
            <div
              key={i}
              data-pack-number={packNumber}
              data-pick-number={i + 1}
              className={`relative h-2 min-w-0 flex-1 first:rounded-l-full last:rounded-r-full ${bg} transition-colors duration-200`}
            />
          );
        })}
      </div>
    </div>
  );
}
