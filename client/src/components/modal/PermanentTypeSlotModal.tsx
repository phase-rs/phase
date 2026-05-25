import { useTranslation } from "react-i18next";

import type { CoreType, GameAction, WaitingFor } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { CardTextboxPreview } from "./CardTextboxPreview.tsx";
import { DialogShell } from "./DialogShell.tsx";

type SlotChoice = Extract<WaitingFor, { type: "ChoosePermanentTypeSlot" }>;

export function PermanentTypeSlotModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "ChoosePermanentTypeSlot") return null;
  if (!canActForWaitingState) return null;

  const data = waitingFor.data as SlotChoice["data"];

  return (
    <SlotChoiceContent
      objectId={data.object_id}
      availableSlots={data.available_slots}
      dispatch={dispatch}
    />
  );
}

function SlotChoiceContent({
  objectId,
  availableSlots,
  dispatch,
}: {
  objectId: number;
  availableSlots: CoreType[];
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const { t } = useTranslation("game");
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const cardName = obj.name;

  return (
    <DialogShell
      eyebrow={t("permanentTypeSlot.eyebrow")}
      title={t("permanentTypeSlot.title")}
      subtitle={t("permanentTypeSlot.subtitle", { name: cardName })}
    >
      <div className="px-3 pt-3 lg:px-5 lg:pt-4">
        <CardTextboxPreview cardName={cardName} />
      </div>
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        {availableSlots.map((slot) => (
          <button
            key={slot}
            onClick={() =>
              dispatch({
                type: "ChoosePermanentTypeSlot",
                data: { slot },
              })
            }
            className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
          >
            <span className="font-semibold text-white">{slot}</span>
          </button>
        ))}
      </div>
    </DialogShell>
  );
}
