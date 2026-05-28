import { useTranslation } from "react-i18next";

import type { WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type TributeChoice = Extract<WaitingFor, { type: "TributeChoice" }>;

/**
 * CR 702.104a: Tribute pay/decline prompt. The controller's opponent-pick
 * (phase 1) is handled upstream by the generic `NamedChoice { Opponent }`
 * flow; this modal covers phase 2 — the chosen opponent decides whether to
 * place N +1/+1 counters on the entering creature. The engine persists the
 * outcome as `ChosenAttribute::TributeOutcome` so the companion
 * "if tribute wasn't paid" trigger (CR 702.104b) reads the decision.
 *
 * Reuses `GameAction::DecideOptionalEffect { accept }` per the engine
 * contract — no new action variant needed.
 */
export function TributeModal() {
  const { t } = useTranslation("game");
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameDispatch();
  const rawSourceName = useGameStore(
    (s) =>
      waitingFor?.type === "TributeChoice"
        ? s.gameState?.objects[waitingFor.data.source_id]?.name ?? null
        : null,
  );

  if (waitingFor?.type !== "TributeChoice") return null;
  if (!canActForWaitingState) return null;

  const sourceName = rawSourceName ?? t("tribute.sourceFallback");
  const data = waitingFor.data as TributeChoice["data"];
  const counters = data.count;

  return (
    <ChoiceModal
      title={t("tribute.title", { name: sourceName })}
      subtitle={t("tribute.subtitle", { count: counters, name: sourceName })}
      previewCardName={rawSourceName ?? undefined}
      previewObjectId={data.source_id}
      options={[
        {
          id: "pay",
          label: t("tribute.payLabel"),
          description: t("tribute.payDescription", { count: counters, name: sourceName }),
        },
        {
          id: "decline",
          label: t("tribute.declineLabel"),
          description: t("tribute.declineDescription"),
        },
      ]}
      onChoose={(id) =>
        dispatch({ type: "DecideOptionalEffect", data: { accept: id === "pay" } })
      }
    />
  );
}
