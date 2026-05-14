import type { GameAction, ManaCost } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ManaCostSymbols } from "../mana/ManaCostSymbols.tsx";
import { CardTextboxPreview } from "./CardTextboxPreview.tsx";
import { DialogShell } from "./DialogShell.tsx";

export function OverloadCostModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "OverloadCostChoice") return null;
  if (!canActForWaitingState) return null;

  // Discriminated-union narrowing makes `waitingFor.data` the correct shape.
  const { object_id, normal_cost, overload_cost } = waitingFor.data;

  return (
    <OverloadCostContent
      objectId={object_id}
      normalCost={normal_cost}
      overloadCost={overload_cost}
      dispatch={dispatch}
    />
  );
}

function OverloadCostContent({
  objectId,
  normalCost,
  overloadCost,
  dispatch,
}: {
  objectId: number;
  normalCost: ManaCost;
  overloadCost: ManaCost;
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const cardName = obj.name;

  return (
    <DialogShell
      eyebrow="Overload"
      title="Choose Casting Cost"
      subtitle={`Cast ${cardName} normally targeting a single permanent, or pay its Overload cost to affect all valid targets.`}
    >
      <div className="px-3 pt-3 lg:px-5 lg:pt-4">
        <CardTextboxPreview cardName={cardName} />
      </div>
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        <button
          onClick={() =>
            dispatch({ type: "ChooseOverloadCost", data: { choice: { type: "Normal" } } })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
        >
          <span className="font-semibold text-white">Cast Normally</span>
          <span className="ml-2"><ManaCostSymbols cost={normalCost} /></span>
        </button>
        <button
          onClick={() =>
            dispatch({ type: "ChooseOverloadCost", data: { choice: { type: "Overload" } } })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-emerald-400/30"
        >
          <span className="font-semibold text-white">Cast with Overload</span>
          <span className="ml-2"><ManaCostSymbols cost={overloadCost} /></span>
        </button>
      </div>
    </DialogShell>
  );
}
