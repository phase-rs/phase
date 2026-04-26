import type { GameAction, ManaCost, WaitingFor } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ManaCostSymbols } from "../mana/ManaCostSymbols.tsx";
import { DialogShell } from "./DialogShell.tsx";

type WarpCostChoice = Extract<WaitingFor, { type: "WarpCostChoice" }>;

export function WarpCostModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "WarpCostChoice") return null;
  if (!canActForWaitingState) return null;

  const data = waitingFor.data as WarpCostChoice["data"];

  return (
    <WarpCostContent
      objectId={data.object_id}
      normalCost={data.normal_cost}
      warpCost={data.warp_cost}
      dispatch={dispatch}
    />
  );
}

function WarpCostContent({
  objectId,
  normalCost,
  warpCost,
  dispatch,
}: {
  objectId: number;
  normalCost: ManaCost;
  warpCost: ManaCost;
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const cardName = obj.name;

  return (
    <DialogShell
      eyebrow="Warp"
      title="Choose Casting Cost"
      subtitle={`Cast ${cardName} normally or use its Warp cost.`}
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        <button
          onClick={() =>
            dispatch({ type: "ChooseWarpCost", data: { use_warp: false } })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
        >
          <span className="font-semibold text-white">Cast Normally</span>
          <span className="ml-2"><ManaCostSymbols cost={normalCost} /></span>
        </button>
        <button
          onClick={() =>
            dispatch({ type: "ChooseWarpCost", data: { use_warp: true } })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-amber-400/30"
        >
          <span className="font-semibold text-white">Cast with Warp</span>
          <span className="ml-2"><ManaCostSymbols cost={warpCost} /></span>
          <span className="ml-1 text-xs text-slate-400">(exiles at end step)</span>
        </button>
      </div>
    </DialogShell>
  );
}
