import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import type { CardType, GameAction, ManaCost, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { formatTypeLine } from "../../viewmodel/cardProps.ts";
import { ManaCostPips } from "../mana/ManaCostPips.tsx";
import { DialogShell } from "./DialogShell.tsx";

type ModalFaceChoice = Extract<WaitingFor, { type: "ModalFaceChoice" }>;
type ChooseModalFaceAction = Extract<GameAction, { type: "ChooseModalFace" }>;

export function ModalFaceModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const legalActions = useGameStore((s) => s.legalActions);
  const dispatch = useGameDispatch();

  if (waitingFor?.type !== "ModalFaceChoice") return null;
  if (!canActForWaitingState) return null;

  const data = waitingFor.data as ModalFaceChoice["data"];

  // CR 712.11c: Only the face that will be face up on the stack is evaluated to
  // determine if it can be cast. The engine has already done that evaluation and
  // publishes the outcome as the legal `ChooseModalFace` actions, so this overlay
  // displays that result and dispatches the engine's own action verbatim. Never
  // rebuild the action or re-derive affordability here.
  const frontAction = legalActions.find(
    (action): action is ChooseModalFaceAction =>
      action.type === "ChooseModalFace" && !action.data.back_face,
  );
  const backAction = legalActions.find(
    (action): action is ChooseModalFaceAction =>
      action.type === "ChooseModalFace" && action.data.back_face,
  );

  return (
    <ModalFaceContent
      objectId={data.object_id}
      frontAction={frontAction}
      backAction={backAction}
      dispatch={dispatch}
    />
  );
}

/** A land face is put onto the battlefield (CR 712.12 play-land special action);
 * a spell face is cast onto the stack. The verb shown mirrors that distinction. */
function faceLabel(
  cardTypes: CardType | undefined,
  name: string,
  t: TFunction<"game">,
): string {
  return cardTypes?.core_types.includes("Land")
    ? t("modalFace.play", { name })
    : t("modalFace.cast", { name });
}

function ModalFaceContent({
  objectId,
  frontAction,
  backAction,
  dispatch,
}: {
  objectId: number;
  frontAction: ChooseModalFaceAction | undefined;
  backAction: ChooseModalFaceAction | undefined;
  dispatch: (action: GameAction) => Promise<void>;
}) {
  const { t } = useTranslation("game");
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const front = {
    name: obj.name,
    cost: obj.mana_cost as ManaCost | undefined,
    types: obj.card_types as CardType | undefined,
  };
  const back = {
    name: obj.back_face?.name ?? t("modalFace.backFaceFallback"),
    cost: obj.back_face?.mana_cost as ManaCost | undefined,
    types: obj.back_face?.card_types as CardType | undefined,
  };

  return (
    <DialogShell
      eyebrow={t("modalFace.eyebrow")}
      title={t("modalFace.title")}
      subtitle={t("modalFace.subtitle")}
      previewObjectId={objectId}
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        {frontAction && (
          <FaceButton
            face={front}
            label={t("modalFace.labelFront")}
            accent="hover:ring-cyan-400/30"
            onClick={() => dispatch(frontAction)}
          />
        )}
        {backAction && (
          <FaceButton
            face={back}
            label={t("modalFace.labelBack")}
            accent="hover:ring-amber-400/30"
            onClick={() => dispatch(backAction)}
          />
        )}
      </div>
    </DialogShell>
  );
}

function FaceButton({
  face,
  label,
  accent,
  onClick,
}: {
  face: { name: string; cost?: ManaCost; types?: CardType };
  label: string;
  accent: string;
  onClick: () => void;
}) {
  const { t } = useTranslation("game");
  const typeLine = face.types ? formatTypeLine(face.types) : "";
  return (
    <button
      onClick={onClick}
      className={`rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 ${accent}`}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="font-semibold text-white">
          {faceLabel(face.types, face.name, t)}
        </span>
        {face.cost && <ManaCostPips cost={face.cost} size="sm" />}
      </div>
      <div className="mt-0.5 flex items-center gap-2 text-xs text-slate-400">
        {typeLine && <span>{typeLine}</span>}
        <span className="ml-auto uppercase tracking-wide text-slate-500">{label}</span>
      </div>
    </button>
  );
}
