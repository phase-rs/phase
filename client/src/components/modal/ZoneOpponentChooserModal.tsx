import { useTranslation } from "react-i18next";

import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
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
 * CR 800.4g: with purpose `SubstituteChooser`, the player who would make a
 * pending pick has left the game, and this player elects who makes it.
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
  const [title, subtitle] =
    purpose === "PerPlayerChoiceOrder"
      ? [
          t("zoneOpponentChooser.orderTitle", "Choose Next Player"),
          t(
            "zoneOpponentChooser.orderSubtitle",
            "Choose whose selection to make next.",
          ),
        ]
      : purpose === "SubstituteChooser"
        ? [
            t("zoneOpponentChooser.substituteTitle", "Choose Player"),
            t(
              "zoneOpponentChooser.substituteSubtitle",
              "Choose who makes this choice.",
            ),
          ]
        : [
            t("zoneOpponentChooser.title", "Choose Opponent"),
            t(
              "zoneOpponentChooser.subtitle",
              "Choose which opponent makes the choice.",
            ),
          ];

  return (
    <ChoiceModal
      title={title}
      subtitle={subtitle}
      options={candidates.map((candidate) => ({
        id: String(candidate),
        label:
          candidate === player
            ? t("player.you", "You")
            : getOpponentDisplayName(candidate),
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
