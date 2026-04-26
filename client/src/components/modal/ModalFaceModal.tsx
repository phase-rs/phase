import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

type ModalFaceChoice = Extract<WaitingFor, { type: "ModalFaceChoice" }>;

export function ModalFaceModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "ModalFaceChoice") return null;
  if (!canActForWaitingState) return null;

  const data = waitingFor.data as ModalFaceChoice["data"];

  return <ModalFaceContent objectId={data.object_id} dispatch={dispatch} />;
}

function ModalFaceContent({
  objectId,
  dispatch,
}: {
  objectId: number;
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const frontName = obj.name;
  const backName = obj.back_face?.name ?? "Back Face";

  return (
    <DialogShell
      eyebrow="Modal DFC"
      title="Choose a Face"
      subtitle="Play as the front or back land face."
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        <button
          onClick={() =>
            dispatch({ type: "ChooseModalFace", data: { back_face: false } })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
        >
          <span className="font-semibold text-white">Play {frontName}</span>
          <span className="ml-2 text-xs text-slate-400">(Front)</span>
        </button>
        <button
          onClick={() =>
            dispatch({ type: "ChooseModalFace", data: { back_face: true } })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-amber-400/30"
        >
          <span className="font-semibold text-white">Play {backName}</span>
          <span className="ml-2 text-xs text-slate-400">(Back)</span>
        </button>
      </div>
    </DialogShell>
  );
}
