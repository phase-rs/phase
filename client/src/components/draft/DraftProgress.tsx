import type { DraftProgressFields } from "../../adapter/draft-adapter";
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
  // one engine value instead of another.
  const {
    current_pack_number,
    pick_number,
    pack_pick_steps,
    pick_steps_per_pack,
    pack_set_codes,
    source,
    pack_count,
    pass_direction,
  } = view;
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
              <PackSegment
                pickCount={packSteps(packIdx)}
                filledPicks={isComplete ? packSteps(packIdx) : isCurrent ? pick_number : 0}
                packNumber={packIdx + 1}
                isCurrent={isCurrent}
                setCode={
                  chaosCurrentPackCode !== undefined
                    ? (isCurrent ? chaosCurrentPackCode ?? undefined : undefined)
                    : (mixedSets ? pack_set_codes?.[packIdx] : undefined)
                }
              />
            </div>
          );
        })}
      </div>
      <div className="shrink-0 text-xs tabular-nums text-white/45">
        <span className="font-semibold text-white">{pick_number + 1}</span>
        <span>/{packSteps(current_pack_number)}</span>
      </div>
    </div>
  );
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
