import { useDraftStore } from "../../stores/draftStore";

export function DraftProgress() {
  const view = useDraftStore((s) => s.view);

  if (!view) return null;

  const { current_pack_number, pick_number, cards_per_pack, pack_count, pass_direction } = view;
  const directionArrow = pass_direction === "Left" ? "←" : "→";

  return (
    <div className="flex items-center gap-4 rounded-[16px] border border-white/10 bg-black/18 px-4 py-2.5 backdrop-blur-md">
      <div className="flex min-w-0 flex-1 items-center gap-1.5">
        {Array.from({ length: pack_count }, (_, packIdx) => {
          const isComplete = packIdx < current_pack_number;
          const isCurrent = packIdx === current_pack_number;

          return (
            <div key={packIdx} className="flex min-w-0 flex-1 items-center gap-1.5">
              {packIdx > 0 && (
                <span className="shrink-0 text-[10px] text-white/20">{directionArrow}</span>
              )}
              <PackSegment
                pickCount={cards_per_pack}
                filledPicks={isComplete ? cards_per_pack : isCurrent ? pick_number : 0}
                isCurrent={isCurrent}
              />
            </div>
          );
        })}
      </div>

      <div className="shrink-0 text-xs tabular-nums text-white/45">
        <span className="font-semibold text-white">{pick_number + 1}</span>
        <span>/{cards_per_pack}</span>
      </div>
    </div>
  );
}

function PackSegment({
  pickCount,
  filledPicks,
  isCurrent,
}: {
  pickCount: number;
  filledPicks: number;
  isCurrent: boolean;
}) {
  return (
    <div className="flex min-w-0 flex-1 gap-px">
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
            className={`h-2 min-w-0 flex-1 first:rounded-l-full last:rounded-r-full ${bg} transition-colors duration-200`}
          />
        );
      })}
    </div>
  );
}
