import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

type CascadeChoiceState = Extract<WaitingFor, { type: "CascadeChoice" }>;

/**
 * CR 702.85a: Cascade — when a cascade-source spell finds an eligible nonland
 * card with mana value strictly less than the source's mana value, the caster
 * may cast it without paying its mana cost or decline. Declining shuffles the
 * hit and all misses to the bottom of the library in a random order.
 */
export function CascadeChoiceModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "CascadeChoice") return null;
  if (!canActForWaitingState) return null;

  const data = waitingFor.data as CascadeChoiceState["data"];

  return (
    <CascadeChoiceContent
      hitCardId={data.hit_card}
      missCount={data.exiled_misses.length}
      sourceMv={data.source_mv}
      dispatch={dispatch}
    />
  );
}

function CascadeChoiceContent({
  hitCardId,
  missCount,
  sourceMv,
  dispatch,
}: {
  hitCardId: number;
  missCount: number;
  sourceMv: number;
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const obj = useGameStore((s) => s.gameState?.objects[hitCardId]);

  if (!obj) return null;

  return (
    <DialogShell
      eyebrow="Cascade"
      title={`Cast ${obj.name}?`}
      subtitle={`Cascade exiled ${obj.name} (mana value below ${sourceMv}). Cast it without paying its mana cost, or decline and shuffle all ${missCount + 1} exiled cards to the bottom of your library.`}
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        <button
          onClick={() =>
            dispatch({
              type: "CascadeChoice",
              data: { choice: { type: "Cast" } },
            })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
        >
          <span className="font-semibold text-white">Cast {obj.name}</span>
          <span className="ml-2 text-xs text-slate-400">(without paying its mana cost)</span>
        </button>
        <button
          onClick={() =>
            dispatch({
              type: "CascadeChoice",
              data: { choice: { type: "Decline" } },
            })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-amber-400/30"
        >
          <span className="font-semibold text-white">Decline</span>
          <span className="ml-2 text-xs text-slate-400">
            (shuffle all exiled cards to the bottom)
          </span>
        </button>
      </div>
    </DialogShell>
  );
}
