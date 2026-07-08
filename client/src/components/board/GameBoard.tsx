import { memo, useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useCanActForWaitingState, usePerspectivePlayerId, usePlayerId } from "../../hooks/usePlayerId.ts";
import { sortCreaturesForBlockers } from "../../viewmodel/blockerSorting.ts";
import { isManaObjectAction } from "../../viewmodel/cardActionChoice.ts";
import {
  buildPlayerBattlefieldView,
  getBoardChoiceView,
  getBattlefieldSacrificeChoice,
  getWaitingForObjectChoiceIds,
  getOpponentIds,
  getSeatCount,
  isSplitBoardActive,
  isOneOnOne,
  resolveFocusedOpponent,
} from "../../viewmodel/gameStateView.ts";
import { BoardInteractionContext } from "./BoardInteractionContext.tsx";
import { ArchenemyPanel } from "./ArchenemyPanel.tsx";
import { CombatLine } from "./CombatLine.tsx";
import { OpponentSeatPane } from "./OpponentSeatPane.tsx";
import { PlayerArea } from "./PlayerArea.tsx";
import { PlanechasePanel } from "./PlanechasePanel.tsx";
import { DraggableWidget } from "../flexlayout/DraggableWidget.tsx";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";

interface GameBoardProps {
  oppHud?: React.ReactNode;
  playerHud?: React.ReactNode;
  showOpponentCards?: boolean;
  onKickPlayer?: (playerId: PlayerId) => void;
  onViewZone?: (zone: "graveyard" | "exile" | "library", playerId: PlayerId) => void;
}

export const GameBoard = memo(function GameBoard({
  oppHud,
  playerHud,
  showOpponentCards = false,
  onKickPlayer,
  onViewZone = () => {},
}: GameBoardProps) {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const multiplayerBoardLayout = usePreferencesStore((s) => s.multiplayerBoardLayout);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const localPlayerId = usePlayerId();
  const myId = usePerspectivePlayerId();
  const canActForWaitingState = useCanActForWaitingState();

  // Track which opponent is focused (expanded) in multiplayer
  const focusedOpponent = useUiStore((s) => s.focusedOpponent) as PlayerId | null;

  const opponents = useMemo(() => {
    return getOpponentIds(gameState, myId);
  }, [gameState, myId]);

  const focusedId = resolveFocusedOpponent(focusedOpponent, opponents);
  const playerBattlefieldView = useMemo(
    () => buildPlayerBattlefieldView(gameState, myId),
    [gameState, myId],
  );
  const focusedBattlefieldView = useMemo(
    () => (focusedId == null ? null : buildPlayerBattlefieldView(gameState, focusedId)),
    [gameState, focusedId],
  );
  const opponentBattlefieldViews = useMemo(() => {
    return new Map(
      opponents.map((opponentId) => [
        opponentId,
        buildPlayerBattlefieldView(gameState, opponentId),
      ]),
    );
  }, [gameState, opponents]);
  const splitBoardActive = isSplitBoardActive(multiplayerBoardLayout, getSeatCount(gameState));

  const sortedPlayerCreatures = useMemo(() => {
    if (splitBoardActive || !focusedBattlefieldView) return undefined;
    return sortCreaturesForBlockers(
      playerBattlefieldView.creatures,
      focusedBattlefieldView.creatures,
      blockerAssignments,
    );
  }, [splitBoardActive, playerBattlefieldView, focusedBattlefieldView, blockerAssignments]);

  const boardInteractionState = useMemo(() => {
    const validTargetObjectIds = new Set<number>();
    const validAttackerIds = new Set<number>();
    const activatableObjectIds = new Set<number>();
    const boardChoiceObjectIds = new Set<number>();
    const manaTappableObjectIds = new Set<number>();
    const selectableSacrificeObjectIds = new Set<number>();
    const selectableManaCostCreatureIds = new Set<number>();
    const undoableTapObjectIds = new Set<number>();
    const committedAttackerIds = new Set<number>();
    const incomingAttackerCounts = new Map<number, number>();

    if (gameState?.combat?.attackers) {
      for (const attacker of gameState.combat.attackers) {
        committedAttackerIds.add(attacker.object_id);
        // Accumulate incoming-attack counts for permanent targets (Planeswalker,
        // Battle). Player targets are handled via HUD `underAttack` treatment.
        const t = attacker.attack_target;
        if (t.type === "Planeswalker" || t.type === "Battle") {
          incomingAttackerCounts.set(t.data, (incomingAttackerCounts.get(t.data) ?? 0) + 1);
        }
      }
    }

    // The undo (UntapLandForMana) is legal only in the three WaitingFor states
    // whose `apply` match arms accept it: Priority (engine.rs:1345), ManaPayment
    // (engine.rs:2705 — un-tap a land mid-cost-payment to change the mana mix),
    // and UnlessPayment (engine.rs:2359 — same, during a "pay unless" choice).
    // Note UnlessPaymentChooseCost is NOT accepted, so it stays excluded. When a
    // mana ability instead pauses mid-resolution for a mandatory choice (e.g.
    // ChooseManaColor for an AnyOneColor land), the source is already in
    // `lands_tapped_for_mana` but the engine is in none of those states —
    // surfacing the undo affordance there produces a rejected dispatch when the
    // tapped land is clicked. Gate the affordance on these states so it matches
    // engine legality exactly.
    const undoLegal =
      waitingFor?.type === "Priority"
      || waitingFor?.type === "ManaPayment"
      || waitingFor?.type === "UnlessPayment";
    if (undoLegal && gameState?.lands_tapped_for_mana?.[localPlayerId]) {
      for (const objectId of gameState.lands_tapped_for_mana[localPlayerId]) {
        undoableTapObjectIds.add(objectId);
      }
    }

    if (waitingFor?.type === "DeclareAttackers") {
      for (const objectId of waitingFor.data.valid_attacker_ids ?? []) {
        validAttackerIds.add(objectId);
      }
    }

    for (const objectId of getWaitingForObjectChoiceIds(waitingFor)) {
      validTargetObjectIds.add(objectId);
    }

    const sacrificeChoice = getBattlefieldSacrificeChoice(waitingFor);
    if (sacrificeChoice && canActForWaitingState) {
      for (const objectId of sacrificeChoice.objectIds) {
        selectableSacrificeObjectIds.add(objectId);
      }
    }

    const boardChoice = getBoardChoiceView(waitingFor, gameState?.objects);
    if (boardChoice && canActForWaitingState) {
      for (const objectId of boardChoice.objectIds) {
        boardChoiceObjectIds.add(objectId);
      }
    }

    if (waitingFor?.type === "EquipTarget") {
      for (const objectId of waitingFor.data.valid_targets) {
        validTargetObjectIds.add(objectId);
      }
    }

    if (waitingFor?.type === "PayCost" && waitingFor.data.kind.type === "TapCreatures") {
      for (const objectId of waitingFor.data.choices) {
        selectableManaCostCreatureIds.add(objectId);
      }
    }

    if (!gameState?.objects) {
      return {
        activatableObjectIds,
        boardChoiceObjectIds,
        committedAttackerIds,
        incomingAttackerCounts,
        manaTappableObjectIds,
        selectableSacrificeObjectIds,
        selectableManaCostCreatureIds,
        undoableTapObjectIds,
        validAttackerIds,
        validTargetObjectIds,
      };
    }

    const playerCanAct =
      waitingFor != null
      && (
        (waitingFor.type === "Priority" && canActForWaitingState)
        || (waitingFor.type === "ManaPayment" && canActForWaitingState)
        || (waitingFor.type === "UnlessPayment" && canActForWaitingState)
        // CR 118.12a: Disjunctive unless-cost — same input enablement as
        // UnlessPayment (player chooses among sub-costs).
        || (waitingFor.type === "UnlessPaymentChooseCost" && canActForWaitingState)
      );

    if (waitingFor?.type === "Priority" && canActForWaitingState) {
      // The engine owns the "which permanent does this action act on" mapping
      // via GameAction::source_object(), exposed as `legalActionsByObject`.
      // The cyan activatable ring surfaces battlefield permanents with at
      // least one non-mana action; mana abilities are handled by the separate
      // mana-tappable ring below. This iteration is variant-agnostic — adding
      // a future keyword activation requires zero frontend changes.
      for (const [idStr, actions] of Object.entries(legalActionsByObject)) {
        const objectId = Number(idStr);
        const object = gameState.objects[objectId];
        if (!object) continue;
        const hasNonManaAction = actions.some((action) => !isManaObjectAction(action, object));
        if (hasNonManaAction) {
          activatableObjectIds.add(objectId);
        }
      }
    }

    if (playerCanAct) {
      for (const [idStr, actions] of Object.entries(legalActionsByObject)) {
        const objectId = Number(idStr);
        const object = gameState.objects[objectId];
        if (!object) continue;
        if (actions.some((action) => isManaObjectAction(action, object))) {
          manaTappableObjectIds.add(objectId);
        }
      }
    }

    return {
      activatableObjectIds,
      boardChoiceObjectIds,
      committedAttackerIds,
      incomingAttackerCounts,
      manaTappableObjectIds,
      selectableSacrificeObjectIds,
      selectableManaCostCreatureIds,
      undoableTapObjectIds,
      validAttackerIds,
      validTargetObjectIds,
    };
  }, [canActForWaitingState, gameState, legalActionsByObject, localPlayerId, waitingFor]);

  if (!gameState) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <span className="text-gray-500">{t("board.waitingForGame")}</span>
      </div>
    );
  }

  // 1v1 layout is a property of the game's seat count, not of how many
  // opponents are currently alive — eliminations would otherwise flip a
  // 3+ player game into the 1v1 inline-pill layout and cram the multi-tab
  // OpponentHud rail into PlayerArea's small `hud` slot.
  const is1v1 = isOneOnOne(gameState);

  return (
    <BoardInteractionContext.Provider value={boardInteractionState}>
      <div className="relative flex min-h-0 min-w-0 flex-1 flex-col">
        <PlanechasePanel />
        <ArchenemyPanel />
        {/* Opponent area */}
        {is1v1 ? (
          opponents[0] != null ? (
            <PlayerArea
              battlefieldView={focusedBattlefieldView ?? undefined}
              playerId={opponents[0]}
              mode="focused"
              hud={oppHud}
            />
          ) : (
            // 1v1 game where the sole opponent has been eliminated. The
            // GameOver modal mounts on the same state, but renders one
            // tick later; guard so we don't index `gameState.players`
            // with `undefined` in the interim.
            <div className="flex flex-1 items-center justify-center" />
          )
        ) : splitBoardActive ? (
          <div className="flex min-h-0 basis-[60%] flex-col overflow-visible pt-[var(--game-split-safe-top,0px)]">
            {opponents.length > 0 ? (
              <div
                className="grid min-h-0 min-w-0 flex-1 items-stretch gap-1 overflow-visible px-1"
                style={{ gridTemplateColumns: `repeat(${opponents.length}, minmax(0, 1fr))` }}
              >
                {opponents.map((opponentId) => (
                  <OpponentSeatPane
                    key={opponentId}
                    playerId={opponentId}
                    battlefieldView={
                      opponentBattlefieldViews.get(opponentId)
                      ?? buildPlayerBattlefieldView(gameState, opponentId)
                    }
                    showCards={showOpponentCards}
                    onKickPlayer={onKickPlayer}
                    onViewZone={onViewZone}
                  />
                ))}
              </div>
            ) : (
              <div className="flex flex-1 items-center justify-center">
                <span className="text-xs text-gray-600">{t("board.clickOpponent")}</span>
              </div>
            )}
          </div>
        ) : (
          <div className="flex min-h-0 flex-1 flex-col">
            {/* Keep opponent controls above overflowing command-zone cards.
                The multiplayer opponent HUD is the table-size-keyed widget —
                repositioning it stores under the "multiplayer" slot, distinct
                from the 1v1 opponent HUD (wired in PlayerArea). */}
            <DraggableWidget
              target={{ kind: "opponentHud", tableSize: "multiplayer" }}
              flexZone="opponentHud"
              className="relative z-40 shrink-0"
            >
              {oppHud}
            </DraggableWidget>
            {focusedId != null ? (
              <PlayerArea
                battlefieldView={focusedBattlefieldView ?? undefined}
                playerId={focusedId}
                mode="focused"
              />
            ) : (
              <div className="flex flex-1 items-center justify-center">
                <span className="text-xs text-gray-600">{t("board.clickOpponent")}</span>
              </div>
            )}
          </div>
        )}

        <CombatLine />

        <PlayerArea
          battlefieldView={playerBattlefieldView}
          playerId={myId}
          mode="full"
          creatureOverride={sortedPlayerCreatures}
          hud={playerHud}
        />
      </div>
    </BoardInteractionContext.Provider>
  );
});
