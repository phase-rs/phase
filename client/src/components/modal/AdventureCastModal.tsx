import { useTranslation } from "react-i18next";

import type { GameAction } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

type ChooseAdventureFaceAction = Extract<GameAction, { type: "ChooseAdventureFace" }>;

export function AdventureCastModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const legalActions = useGameStore((s) => s.legalActions);
  const dispatch = useGameDispatch();

  if (waitingFor?.type !== "CastOffer" || waitingFor.data.kind.type !== "Adventure") return null;
  if (!canActForWaitingState) return null;

  const kind = waitingFor.data.kind;
  const creatureAction = legalActions.find(
    (action): action is ChooseAdventureFaceAction =>
      action.type === "ChooseAdventureFace" && action.data.creature,
  );
  const adventureAction = legalActions.find(
    (action): action is ChooseAdventureFaceAction =>
      action.type === "ChooseAdventureFace" && !action.data.creature,
  );

  return (
    <AdventureCastContent
      objectId={kind.object_id}
      creatureAction={creatureAction}
      adventureAction={adventureAction}
      dispatch={dispatch}
    />
  );
}

function AdventureCastContent({
  objectId,
  creatureAction,
  adventureAction,
  dispatch,
}: {
  objectId: number;
  creatureAction: ChooseAdventureFaceAction | undefined;
  adventureAction: ChooseAdventureFaceAction | undefined;
  dispatch: (action: GameAction) => Promise<void>;
}) {
  const { t } = useTranslation("game");
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const creatureName = obj.name;
  const adventureName = obj.back_face?.name ?? t("adventureCast.adventureFallback");

  return (
    <DialogShell
      eyebrow={t("adventureCast.eyebrow")}
      title={t("adventureCast.title")}
      subtitle={t("adventureCast.subtitle")}
      previewObjectId={objectId}
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        {creatureAction && (
          <button
            onClick={() => dispatch(creatureAction)}
            className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
          >
            <span className="font-semibold text-white">
              {t("adventureCast.castNamed", { name: creatureName })}
            </span>
            <span className="ml-2 text-xs text-slate-400">
              {t("adventureCast.creatureTag")}
            </span>
          </button>
        )}
        {adventureAction && (
          <button
            onClick={() => dispatch(adventureAction)}
            className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-amber-400/30"
          >
            <span className="font-semibold text-white">
              {t("adventureCast.castNamed", { name: adventureName })}
            </span>
            <span className="ml-2 text-xs text-slate-400">
              {t("adventureCast.adventureTag")}
            </span>
          </button>
        )}
      </div>
    </DialogShell>
  );
}
