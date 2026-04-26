import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

type AdventureCastChoice = Extract<WaitingFor, { type: "AdventureCastChoice" }>;

export function AdventureCastModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "AdventureCastChoice") return null;
  if (!canActForWaitingState) return null;

  const data = waitingFor.data as AdventureCastChoice["data"];

  return <AdventureCastContent objectId={data.object_id} dispatch={dispatch} />;
}

function AdventureCastContent({
  objectId,
  dispatch,
}: {
  objectId: number;
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const creatureName = obj.name;
  const adventureName = obj.back_face?.name ?? "Adventure";

  return (
    <DialogShell
      eyebrow="Adventure"
      title="Choose a Face"
      subtitle="Cast as the creature or as the Adventure spell."
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        <button
          onClick={() =>
            dispatch({ type: "ChooseAdventureFace", data: { creature: true } })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
        >
          <span className="font-semibold text-white">Cast {creatureName}</span>
          <span className="ml-2 text-xs text-slate-400">(Creature)</span>
        </button>
        <button
          onClick={() =>
            dispatch({ type: "ChooseAdventureFace", data: { creature: false } })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-amber-400/30"
        >
          <span className="font-semibold text-white">Cast {adventureName}</span>
          <span className="ml-2 text-xs text-slate-400">(Adventure)</span>
        </button>
      </div>
    </DialogShell>
  );
}
