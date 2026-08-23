import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import type { GameAction, GameState, WaitingFor } from "../../adapter/types.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type OptionalEffectWaitingFor = Extract<
  WaitingFor,
  { type: "OptionalEffectChoice" | "OpponentMayChoice" }
>;

interface OptionalEffectModalProps {
  waitingFor: OptionalEffectWaitingFor;
  objects?: GameState["objects"];
  dispatch: (action: GameAction) => void | Promise<void>;
}

export function OptionalEffectModalContent({
  waitingFor,
  objects,
  dispatch,
}: OptionalEffectModalProps) {
  const { t } = useTranslation("game");
  const [remember, setRemember] = useState(false);
  const [sameCard, setSameCard] = useState(false);

  useEffect(() => {
    setRemember(false);
    setSameCard(false);
  }, [waitingFor]);

  const sourceObj = objects?.[waitingFor.data.source_id];
  const sourceName = sourceObj?.name ?? t("optionalEffect.sourceFallback");
  const description = waitingFor.data.description as string | undefined;
  const canRemember =
    waitingFor.type === "OptionalEffectChoice" && waitingFor.data.may_trigger_key != null;
  const sameCardAvailable =
    waitingFor.type === "OptionalEffectChoice" &&
    waitingFor.data.same_card_may_trigger_choice_available === true;

  return (
    <ChoiceModal
      title={t("optionalEffect.title", { name: sourceName })}
      subtitle={description}
      previewCardName={sourceObj?.name}
      previewObjectId={waitingFor.data.source_id}
      options={[
        { id: "accept", label: t("optionalEffect.yes") },
        { id: "decline", label: t("optionalEffect.no") },
      ]}
      onChoose={(id) => {
        const accept = id === "accept";
        if (remember && canRemember) {
          dispatch({
            type: "DecideOptionalEffectAndRemember",
            data: {
              choice: { type: accept ? "Accept" : "Decline" },
              scope: { type: sameCard ? "SameCard" : "ExactInstance" },
            },
          });
          return;
        }
        dispatch({ type: "DecideOptionalEffect", data: { accept } });
      }}
      footer={
        canRemember ? (
          <div className="space-y-2 rounded-[10px] border border-white/8 bg-black/20 px-3 py-2 text-sm text-slate-200">
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={remember}
                onChange={(event) => {
                  setRemember(event.currentTarget.checked);
                  if (!event.currentTarget.checked) setSameCard(false);
                }}
                className="h-4 w-4 accent-cyan-400"
              />
              <span>{t("optionalEffect.dontAskAgain")}</span>
            </label>
            {sameCardAvailable && (
              <label className="flex items-center gap-2">
                <input
                  type="checkbox"
                  checked={sameCard}
                  onChange={(event) => {
                    setSameCard(event.currentTarget.checked);
                    if (event.currentTarget.checked) setRemember(true);
                  }}
                  className="h-4 w-4 accent-cyan-400"
                />
                <span>{t("optionalEffect.dontAskAgainForSameCard")}</span>
              </label>
            )}
          </div>
        ) : undefined
      }
    />
  );
}
