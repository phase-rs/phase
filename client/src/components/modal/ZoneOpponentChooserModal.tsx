import { useTranslation } from "react-i18next";

import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type ZoneOpponentChooserWaitingFor = Extract<
  WaitingFor,
  { type: "ChooseFromZoneOpponentChooser" }
>;

interface ZoneOpponentChooserModalContentProps {
  waitingFor: ZoneOpponentChooserWaitingFor;
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 608.2d: "An opponent chooses …" from a zone in a multiplayer game — the
 * controller decides which opponent makes the choice before the zone choice
 * itself is presented to that opponent (Plargg and Nassari's release notes:
 * "you choose which opponent gets to choose one of the exiled nonland cards").
 *
 * CR 101.4c: with purpose `PerPlayerChoiceOrder`, the single chooser of a
 * "for each player/opponent, choose …" iteration picks whose selection to make
 * next. Candidates may include the chooser themself, labelled "You".
 *
 * Candidates render in the ENGINE-SUPPLIED order: candidate ordering is game
 * ordering and belongs to the engine, so the client must not re-sort it.
 */
export function ZoneOpponentChooserModalContent({
  waitingFor,
  dispatch,
}: ZoneOpponentChooserModalContentProps) {
  const { t } = useTranslation("game");
  const { candidates, player, purpose } = waitingFor.data;
  const choosingOrder = purpose === "PerPlayerChoiceOrder";

  return (
    <ChoiceModal
      title={
        choosingOrder
          ? t("zoneOpponentChooser.orderTitle", "Choose Next Player")
          : t("zoneOpponentChooser.title", "Choose Opponent")
      }
      subtitle={
        choosingOrder
          ? t(
              "zoneOpponentChooser.orderSubtitle",
              "Choose whose selection to make next.",
            )
          : t(
              "zoneOpponentChooser.subtitle",
              "Choose which opponent makes the choice.",
            )
      }
      options={candidates.map((candidate) => ({
        id: String(candidate),
        label: getPlayerDisplayName(candidate, player),
      }))}
      onChoose={(id) => {
        dispatch({
          type: "ChooseZoneOpponentChooser",
          data: { opponent: Number(id) },
        });
      }}
    />
  );
}

export function ZoneOpponentChooserModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.waitingFor);

  if (waitingFor?.type !== "ChooseFromZoneOpponentChooser") return null;
  if (!canActForWaitingState) return null;

  return (
    <ZoneOpponentChooserModalContent
      waitingFor={waitingFor}
      dispatch={dispatch}
    />
  );
}
