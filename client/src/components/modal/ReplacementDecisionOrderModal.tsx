import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { GameAction, GameObject, ObjectId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { objectImageProps } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { CardImage } from "../card/CardImage.tsx";
import { CHOICE_CARD_IMAGE_CLASS } from "./cardChoice/shared.tsx";
import { ChoiceOverlay, ScrollableCardStrip } from "./ChoiceOverlay.tsx";

type ReplacementDecisionOrderWaitingFor = Extract<
  WaitingFor,
  { type: "ReplacementDecisionOrder" }
>;

interface ReplacementDecisionOrderModalContentProps {
  waitingFor: ReplacementDecisionOrderWaitingFor;
  objects: Record<string, GameObject> | undefined;
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 101.4c: the player has several simultaneous replacement decisions and
 * chooses which to make first. Each option is the engine-provided object the
 * decision is about, shown as its own card (hovering inspects that exact
 * object) and captioned with what the engine says it is attached to or what is
 * attached to it, so two objects with the same name can be told apart and a
 * decision about an Aura names the permanent it is on.
 */
export function ReplacementDecisionOrderModalContent({
  waitingFor,
  objects,
  dispatch,
}: ReplacementDecisionOrderModalContentProps) {
  const { t } = useTranslation("game");
  const hoverProps = useInspectHoverProps();
  const nameOf = (object: ObjectId) =>
    objects?.[String(object)]?.name ?? t("replacementDecisionOrder.unknownObject");
  const captionOf = (object: GameObject | undefined) => {
    if (!object) return undefined;
    if (object.attached_to?.type === "Object") {
      return t("replacementDecisionOrder.attachedTo", {
        host: nameOf(object.attached_to.data),
      });
    }
    if (object.attachments.length > 0) {
      return t("replacementDecisionOrder.attachments", {
        names: object.attachments.map(nameOf).join(", "),
      });
    }
    return undefined;
  };

  return (
    <ChoiceOverlay
      title={t("replacementDecisionOrder.title")}
      subtitle={t("replacementDecisionOrder.subtitle")}
    >
      <ScrollableCardStrip>
        {waitingFor.data.objects.map((id, index) => {
          const object = objects?.[String(id)];
          const caption = captionOf(object);
          const name = nameOf(id);
          return (
            <motion.button
              key={id}
              aria-label={caption ? `${name} (${caption})` : name}
              className="relative flex shrink-0 flex-col items-center gap-1 rounded-lg transition hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: 0.85, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6, opacity: 1 }}
              onClick={() => {
                dispatch({ type: "SelectCards", data: { cards: [id] } });
              }}
              {...hoverProps(id)}
            >
              {object ? (
                <CardImage
                  {...objectImageProps(object)}
                  size="normal"
                  className={CHOICE_CARD_IMAGE_CLASS}
                />
              ) : (
                <span className="px-3 py-2 text-sm text-white">{name}</span>
              )}
              {caption && (
                <span className="max-w-[14rem] rounded-full bg-sky-700/95 px-2 py-0.5 text-center text-[11px] font-bold text-white shadow">
                  {caption}
                </span>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

export function ReplacementDecisionOrderModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (waitingFor?.type !== "ReplacementDecisionOrder") return null;
  if (!canActForWaitingState) return null;

  return (
    <ReplacementDecisionOrderModalContent
      waitingFor={waitingFor}
      objects={objects}
      dispatch={dispatch}
    />
  );
}
