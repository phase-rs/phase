import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import type { AttackerInfo, GameObject, ObjectId, WaitingFor } from "../../adapter/types.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { useCanActForWaitingState, usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { GroupedPermanent as GroupedPermanentType } from "../../viewmodel/battlefieldProps";
import {
  boardChoiceMaxSelection,
  boardChoiceSelectedPower,
  buildBoardChoiceAction,
  canConfirmBoardChoice,
  getBoardChoiceView,
  getWaitingForObjectChoiceIds,
  isBoardChoiceImmediate,
  type BoardChoiceView,
} from "../../viewmodel/gameStateView.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { type BlockerAssignments, useUiStore } from "../../stores/uiStore.ts";
import { blockersByAttacker, blockTargetSelection, partitionBlockTargets } from "../../utils/combat.ts";
import { useBoardInteractionState } from "./BoardInteractionContext.tsx";
import { MeldedCardFrame } from "./MeldedCardFrame.tsx";
import { PermanentCard } from "./PermanentCard.tsx";
import { type GroupRenderMode, groupStaggerPx, type BattlefieldRowType } from "./groupRenderMode.ts";

interface GroupedPermanentProps {
  group: GroupedPermanentType;
  rowType: BattlefieldRowType;
  renderMode: GroupRenderMode;
  onExpand: () => void;
}

type PickerContext =
  | { mode: "attackers" | "blockers" | "equip" | "target" | "tap"; eligibleIds: ObjectId[] }
  | { mode: "boardChoice"; eligibleIds: ObjectId[]; choice: BoardChoiceView }
  | { mode: "blockTargets"; eligibleIds: ObjectId[]; blockerId: ObjectId };

const COLLAPSED_PICKER_WIDTH_PX = 208;
const COLLAPSED_PICKER_GAP_PX = 8;
const COLLAPSED_PICKER_VIEWPORT_PADDING_PX = 8;

interface CollapsedPickerPosition {
  top: number | "auto";
  bottom: number | "auto";
  left: number;
  width: number;
  maxHeight: number;
}

function waitingForPlayer(waitingFor: WaitingFor | null | undefined): number | null {
  switch (waitingFor?.type) {
    case "TargetSelection":
    case "DeclareAttackers":
    case "DeclareBlockers":
    case "EquipTarget":
    case "CopyTargetChoice":
    case "ExploreChoice":
    case "PopulateChoice":
    case "ReturnAsAuraTarget":
    case "TriggerTargetSelection":
    case "RetargetChoice":
    case "PayCost":
    case "EffectZoneChoice":
    case "WardSacrificeChoice":
    case "UnlessBounceChoice":
    case "ChooseRingBearer":
    case "BlightChoice":
    case "CrewVehicle":
    case "StationTarget":
    case "SaddleMount":
    case "HarmonizeTapChoice":
    case "KeepWithinTotalPowerChoice":
    case "KeepExactPermanentsChoice":
    case "UntapChoice":
    case "ChooseUntapSubset":
      return waitingFor.data.player;
    default:
      return null;
  }
}

export const GroupedPermanentDisplay = memo(function GroupedPermanentDisplay({
  group,
  rowType,
  renderMode,
  onExpand,
}: GroupedPermanentProps) {
  const { t } = useTranslation("game");
  const [pickerOpen, setPickerOpen] = useState(false);
  const collapsedAnchorRef = useRef<HTMLDivElement | null>(null);
  const playerId = usePlayerId();
  const canActForWaitingState = useCanActForWaitingState();
  const battlefieldCardDisplay = usePreferencesStore((s) => s.battlefieldCardDisplay);
  const combatMode = useUiStore((s) => s.combatMode);
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const setGroupSelectedAttackers = useUiStore((s) => s.setGroupSelectedAttackers);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const pendingBlocker = useUiStore((s) => s.pendingBlocker);
  const setGroupBlockerAssignments = useUiStore((s) => s.setGroupBlockerAssignments);
  const combatClickHandler = useUiStore((s) => s.combatClickHandler);
  const selectedCardIds = useUiStore((s) => s.selectedCardIds);
  const setGroupSelectedCards = useUiStore((s) => s.setGroupSelectedCards);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const gameObjects = useGameStore((s) => s.gameState?.objects);
  const combatAttackers = useGameStore((s) => s.gameState?.combat?.attackers);
  const manaPaymentPreviewSourceIds = useGameStore((s) => s.manaPaymentPreviewSourceIds);
  const {
    boardChoiceObjectIds,
    committedAttackerIds,
    validAttackerIds,
    validTargetObjectIds,
  } = useBoardInteractionState();

  const pickerContext = useMemo<PickerContext | null>(() => {
    if (renderMode !== "collapsed") return null;

    const boardChoice = getBoardChoiceView(waitingFor, gameObjects);
    if (boardChoice && canActForWaitingState) {
      const eligibleIds = group.ids.filter((id) => boardChoiceObjectIds.has(id));
      return eligibleIds.length > 0
        ? { mode: "boardChoice", eligibleIds, choice: boardChoice }
        : null;
    }

    if (waitingForPlayer(waitingFor) !== playerId) return null;

    if (combatMode === "attackers") {
      const eligibleIds = group.ids.filter((id) => validAttackerIds.has(id));
      return eligibleIds.length > 0 ? { mode: "attackers", eligibleIds } : null;
    }

    if (combatMode === "blockers" && waitingFor?.type === "DeclareBlockers" && pendingBlocker !== null) {
      // A pile of ATTACKERS with a blocker pending: assign that blocker onto
      // one or more members (CollapsedGroupPicker's blockTargets mode). Never
      // matches the same group as the own-blockers branch below — a group is
      // per controller, and attackers and this defender's blockers belong to
      // different controllers.
      const validTargetsForPending = new Set(waitingFor.data.valid_block_targets[pendingBlocker] ?? []);
      const eligibleIds = group.ids.filter((id) => validTargetsForPending.has(id));
      if (eligibleIds.length > 0) {
        return { mode: "blockTargets", eligibleIds, blockerId: pendingBlocker };
      }
    }

    if (combatMode === "blockers" && waitingFor?.type === "DeclareBlockers" && combatClickHandler) {
      const validBlockerIds = new Set(waitingFor.data.valid_blocker_ids);
      const eligibleIds = group.ids.filter((id) =>
        validBlockerIds.has(id)
        && (waitingFor.data.valid_block_targets[id]?.length ?? 0) > 0,
      );
      return eligibleIds.length > 0 ? { mode: "blockers", eligibleIds } : null;
    }

    if (waitingFor?.type === "EquipTarget") {
      const validEquipTargetIds = new Set(waitingFor.data.valid_targets);
      const eligibleIds = group.ids.filter((id) =>
        validEquipTargetIds.has(id) && validTargetObjectIds.has(id),
      );
      return eligibleIds.length > 0 ? { mode: "equip", eligibleIds } : null;
    }

    const objectChoiceIds = new Set(getWaitingForObjectChoiceIds(waitingFor));
    const targetEligibleIds = group.ids.filter((id) =>
      objectChoiceIds.has(id) && validTargetObjectIds.has(id),
    );
    if (targetEligibleIds.length > 0) {
      return { mode: "target", eligibleIds: targetEligibleIds };
    }

    if (waitingFor?.type === "PayCost" && waitingFor.data.kind.type === "TapCreatures") {
      const tappableIds = new Set(waitingFor.data.choices);
      const eligibleIds = group.ids.filter((id) => tappableIds.has(id));
      return eligibleIds.length > 0 ? { mode: "tap", eligibleIds } : null;
    }

    return null;
  }, [
    blockerAssignments,
    boardChoiceObjectIds,
    canActForWaitingState,
    combatClickHandler,
    combatMode,
    gameObjects,
    group.ids,
    pendingBlocker,
    playerId,
    renderMode,
    validAttackerIds,
    validTargetObjectIds,
    waitingFor,
  ]);

  useEffect(() => {
    if (renderMode !== "collapsed" || !pickerContext || pickerContext.eligibleIds.length === 0) {
      setPickerOpen(false);
    }
  }, [group.ids, pickerContext, renderMode, waitingFor]);

  const selectedAttackerCount = group.ids.filter((id) => selectedAttackers.includes(id)).length;
  const selectedTapCount = group.ids.filter((id) => selectedCardIds.includes(id)).length;
  const assignedBlockerCount = group.ids.filter((id) => blockerAssignments.has(id)).length;
  const committedAttackerCount = group.ids.filter((id) => committedAttackerIds.has(id)).length;
  const directBlockers = useMemo(
    () => blockersByAttacker(blockerAssignments),
    [blockerAssignments],
  );
  // Distinct pile members with a direct blocker selection. A band-mate can
  // also be blocked by the engine without a direct selection here (CR 702.22h).
  const directBlockTargetCount = combatMode === "blockers"
    ? group.ids.filter((id) => directBlockers.has(id)).length
    : 0;
  const canOpenPicker = pickerContext != null;

  const aggregateRingClass =
    selectedAttackerCount > 0 || assignedBlockerCount > 0 || committedAttackerCount > 0
      ? "ring-2 ring-orange-500 shadow-[0_0_12px_3px_rgba(249,115,22,0.7)]"
      : selectedTapCount > 0
        ? "ring-2 ring-emerald-400 shadow-[0_0_14px_4px_rgba(52,211,153,0.55)]"
        : canOpenPicker
          ? "outline outline-2 outline-black/80 ring-4 ring-lime-300 shadow-[0_0_18px_6px_rgba(190,242,100,0.72),inset_0_0_18px_4px_rgba(190,242,100,0.22)]"
          : "";

  if (renderMode === "single") {
    // SHOULD-FIX #1 (singleton trap): getGroupRenderMode returns "single" for
    // count <= 1, which normally renders no count badge. The ∞ semantics are
    // COUNT-INDEPENDENT (an accepted object-growth pile is ∞ regardless of how many
    // members are currently visible), so a single-member pile must still show ∞.
    if (group.isUnboundedPile) {
      return (
        <div className="relative">
          <PermanentCard objectId={group.ids[0]} />
          <span className="absolute left-1 top-1 z-30 flex h-5 w-5 items-center justify-center rounded-full bg-black/80 text-[10px] font-bold text-white ring-1 ring-gray-500">
            ∞
          </span>
        </div>
      );
    }
    if (group.representative?.isMelded) {
      return (
        <MeldedCardFrame>
          <PermanentCard objectId={group.ids[0]} />
        </MeldedCardFrame>
      );
    }
    return <PermanentCard objectId={group.ids[0]} />;
  }

  if (renderMode === "expanded") {
    return (
      <div className="relative flex flex-wrap items-end gap-1">
        {group.ids.map((id) => (
          <PermanentCard key={id} objectId={id} />
        ))}
        {/* Collapse affordance anchored to the same screen position the
            expand badge occupied before expansion, so users return to a
            stacked group via the spot they clicked to expand it. */}
        <button
          type="button"
          onClick={onExpand}
          className="absolute left-1 top-1 z-30 flex h-5 w-5 items-center justify-center rounded-full bg-black/80 text-[10px] font-bold text-white ring-1 ring-gray-500 transition-colors hover:bg-black"
          aria-label={t("permanent.collapseGroup", { name: group.name })}
          title={t("permanent.collapseGroup", { name: group.name })}
        >
          {group.isUnboundedPile ? "∞" : group.count}
        </button>
      </div>
    );
  }

  if (renderMode === "collapsed") {
    return (
      <div ref={collapsedAnchorRef} className={`relative ${canOpenPicker ? "z-40" : ""}`}>
        <div className={`relative rounded-lg ${aggregateRingClass}`}>
          <PermanentCard
            objectId={group.ids[0]}
            coveredIds={group.ids}
            onPrimaryClickOverride={canOpenPicker ? () => setPickerOpen(true) : undefined}
          />
          {aggregateRingClass && (
            <div
              aria-hidden
              className={`pointer-events-none absolute inset-[-3px] z-30 rounded-xl ${aggregateRingClass}`}
            />
          )}
        </div>
        <button
          type="button"
          onClick={(event) => {
            event.stopPropagation();
            onExpand();
          }}
          className="absolute -left-3 -top-3 z-40 flex h-8 min-w-8 items-center justify-center rounded-full bg-black px-1.5 text-sm font-extrabold text-white ring-2 ring-white/80 shadow-[0_2px_8px_rgba(0,0,0,0.65)] transition-transform hover:scale-105"
          aria-label={t("permanent.expandGroup", { name: group.name })}
        >
          {group.isUnboundedPile ? "∞" : `×${group.count}`}
        </button>
        {canOpenPicker && (
          <button
            type="button"
            onClick={(event) => {
              event.stopPropagation();
              setPickerOpen((open) => !open);
            }}
            className="absolute -bottom-2 left-1/2 z-40 -translate-x-1/2 rounded-full bg-amber-400 px-2 py-0.5 text-[10px] font-extrabold uppercase leading-none text-black ring-1 ring-black/40 shadow-[0_2px_8px_rgba(0,0,0,0.55)] transition-transform hover:scale-105 hover:bg-amber-300"
            aria-label={t("permanent.chooseToken", { name: group.name })}
            aria-expanded={pickerOpen}
          >
            {t("permanent.pick")}
          </button>
        )}
        <CollapsedGroupBadges
          assignedBlockerCount={assignedBlockerCount}
          directBlockTargetCount={directBlockTargetCount}
          committedAttackerCount={committedAttackerCount}
          eligibleCount={pickerContext?.eligibleIds.length ?? 0}
          selectedAttackerCount={selectedAttackerCount}
          selectedTapCount={selectedTapCount}
        />
        {pickerOpen && pickerContext && (
          <CollapsedGroupPicker
            anchorEl={collapsedAnchorRef.current}
            context={pickerContext}
            group={group}
            selectedAttackers={selectedAttackers}
            selectedCardIds={selectedCardIds}
            setGroupSelectedAttackers={setGroupSelectedAttackers}
            setGroupSelectedCards={setGroupSelectedCards}
            waitingFor={waitingFor}
            combatAttackers={combatAttackers}
            blockerAssignments={blockerAssignments}
            setGroupBlockerAssignments={setGroupBlockerAssignments}
            combatClickHandler={combatClickHandler}
            onClose={() => setPickerOpen(false)}
          />
        )}
      </div>
    );
  }

  const staggerPx = groupStaggerPx(rowType);

  return (
    <div
      className="relative"
      style={{
        // Reserve width for staggered cards beyond the first
        paddingRight: `${(group.count - 1) * staggerPx}px`,
      }}
    >
      {/* Each card staggered to the right, last card on top */}
      {group.ids.map((id, i) => (
        <div
          key={id}
          className="absolute top-0"
          style={{
            left: `${i * staggerPx}px`,
            // A preview source must rise above unselected cards in this local
            // stacking context; its own outline cannot escape this wrapper.
            zIndex: manaPaymentPreviewSourceIds.includes(id) ? group.count + i : i,
          }}
        >
          <PermanentCard objectId={id} />
        </div>
      ))}

      {/* Invisible spacer sized to first card for layout */}
      <div
        aria-hidden="true"
        className="pointer-events-none"
        style={
          battlefieldCardDisplay === "art_crop"
            ? { width: "var(--art-crop-w)", height: "var(--art-crop-h)" }
            : { width: "var(--card-w)", height: "var(--card-h)" }
        }
      />

      {/* Count badge — orange glow during blocker mode to hint at expansion */}
      <button
        type="button"
        onClick={onExpand}
        className={`absolute left-1 top-1 z-30 flex h-5 w-5 items-center justify-center rounded-full bg-black/80 text-[10px] font-bold text-white transition-colors hover:bg-black ${
          combatMode === "blockers"
            ? "ring-2 ring-orange-500 shadow-[0_0_8px_2px_rgba(249,115,22,0.6)]"
            : "ring-1 ring-gray-500"
        }`}
        aria-label={`Expand ${group.name} group`}
      >
        {group.isUnboundedPile ? "∞" : group.count}
      </button>
    </div>
  );
});

interface CollapsedGroupBadgesProps {
  assignedBlockerCount: number;
  directBlockTargetCount: number;
  committedAttackerCount: number;
  eligibleCount: number;
  selectedAttackerCount: number;
  selectedTapCount: number;
}

function CollapsedGroupBadges({
  assignedBlockerCount,
  directBlockTargetCount,
  committedAttackerCount,
  eligibleCount,
  selectedAttackerCount,
  selectedTapCount,
}: CollapsedGroupBadgesProps) {
  const { t } = useTranslation("game");
  const actionCount = selectedAttackerCount || assignedBlockerCount || selectedTapCount;
  return (
    <div className="pointer-events-none absolute -right-2 top-1 z-40 flex flex-col items-end gap-1">
      {eligibleCount > 0 && (
        <span className="rounded bg-amber-500 px-1.5 py-0.5 text-[10px] font-bold leading-none text-black shadow">
          {t("permanent.eligibleCount", { count: eligibleCount })}
        </span>
      )}
      {committedAttackerCount > 0 && (
        <span className="rounded bg-orange-600 px-1.5 py-0.5 text-[10px] font-bold leading-none text-white shadow">
          {t("permanent.attackingCount", { count: committedAttackerCount })}
        </span>
      )}
      {directBlockTargetCount > 0 && (
        <span className="rounded bg-sky-600 px-1.5 py-0.5 text-[10px] font-bold leading-none text-white shadow">
          {t("permanent.directBlockTargetCount", { count: directBlockTargetCount })}
        </span>
      )}
      {actionCount > 0 && (
        <span className="rounded bg-white px-1.5 py-0.5 text-[10px] font-bold leading-none text-black shadow">
          {t("permanent.selectedCount", { count: actionCount })}
        </span>
      )}
    </div>
  );
}

interface CollapsedGroupPickerProps {
  anchorEl: HTMLElement | null;
  context: PickerContext;
  group: GroupedPermanentType;
  selectedAttackers: ObjectId[];
  selectedCardIds: ObjectId[];
  setGroupSelectedAttackers: (groupIds: ObjectId[], selectedIds: ObjectId[]) => void;
  setGroupSelectedCards: (groupIds: ObjectId[], selectedIds: ObjectId[]) => void;
  waitingFor: WaitingFor | null | undefined;
  combatAttackers: AttackerInfo[] | undefined;
  blockerAssignments: BlockerAssignments;
  setGroupBlockerAssignments: (blockerId: ObjectId, groupIds: ObjectId[], attackerIds: ObjectId[]) => void;
  combatClickHandler: ((id: ObjectId) => void) | null;
  onClose: () => void;
}

function CollapsedGroupPicker({
  anchorEl,
  context,
  group,
  selectedAttackers,
  selectedCardIds,
  setGroupSelectedAttackers,
  setGroupSelectedCards,
  waitingFor,
  combatAttackers,
  blockerAssignments,
  setGroupBlockerAssignments,
  combatClickHandler,
  onClose,
}: CollapsedGroupPickerProps) {
  const { t } = useTranslation("game");
  const objects = useGameStore((s) => s.gameState?.objects);
  const [position, setPosition] = useState<CollapsedPickerPosition | null>(null);
  const selectedAttackerCount = context.eligibleIds.filter((id) => selectedAttackers.includes(id)).length;
  const selectedTapCount = context.eligibleIds.filter((id) => selectedCardIds.includes(id)).length;

  const updatePosition = useCallback(() => {
    if (!anchorEl) return;
    const rect = anchorEl.getBoundingClientRect();
    const viewportPadding = COLLAPSED_PICKER_VIEWPORT_PADDING_PX;
    const width = Math.max(
      0,
      Math.min(COLLAPSED_PICKER_WIDTH_PX, window.innerWidth - viewportPadding * 2),
    );
    const left = Math.max(
      viewportPadding,
      Math.min(
        rect.left + rect.width / 2 - width / 2,
        window.innerWidth - width - viewportPadding,
      ),
    );
    const spaceBelow = window.innerHeight - rect.bottom - COLLAPSED_PICKER_GAP_PX - viewportPadding;
    const spaceAbove = rect.top - COLLAPSED_PICKER_GAP_PX - viewportPadding;
    const openUp = spaceAbove > spaceBelow;
    const maxHeight = Math.max(0, openUp ? spaceAbove : spaceBelow);

    setPosition({
      top: openUp ? "auto" : rect.bottom + COLLAPSED_PICKER_GAP_PX,
      bottom: openUp ? window.innerHeight - rect.top + COLLAPSED_PICKER_GAP_PX : "auto",
      left,
      width,
      maxHeight,
    });
  }, [anchorEl]);

  useLayoutEffect(() => {
    updatePosition();
  }, [updatePosition]);

  useEffect(() => {
    if (!anchorEl) return undefined;
    window.addEventListener("resize", updatePosition);
    window.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", updatePosition);
      window.removeEventListener("scroll", updatePosition, true);
    };
  }, [anchorEl, updatePosition]);

  const selectAttackerCount = (count: number) => {
    setGroupSelectedAttackers(group.ids, context.eligibleIds.slice(0, count));
  };

  const selectTapCount = (count: number) => {
    setGroupSelectedCards(group.ids, context.eligibleIds.slice(0, count));
  };

  const tapLimit = useMemo(() => {
    if (waitingFor?.type !== "PayCost" || waitingFor.data.kind.type !== "TapCreatures") {
      return 0;
    }
    const groupIdSet = new Set(group.ids);
    const selectedOutsideGroup = selectedCardIds.filter((id) => !groupIdSet.has(id)).length;
    return Math.min(context.eligibleIds.length, Math.max(0, waitingFor.data.count - selectedOutsideGroup));
  }, [context.eligibleIds.length, group.ids, selectedCardIds, waitingFor]);

  if (!anchorEl || !position) return null;

  return createPortal(
    <div
      className="fixed z-[160] overflow-y-auto overscroll-contain rounded border border-slate-500 bg-slate-950/95 p-2 text-xs text-white shadow-2xl"
      style={{
        top: position.top,
        bottom: position.bottom,
        left: position.left,
        width: position.width,
        maxHeight: position.maxHeight,
      }}
      onPointerDown={(event) => event.stopPropagation()}
      onClick={(event) => event.stopPropagation()}
    >
      <div className="mb-2 flex items-center justify-between gap-2">
        <span className="truncate font-semibold">{group.name}</span>
        <button
          type="button"
          className="rounded px-1 text-slate-300 hover:bg-slate-800 hover:text-white"
          onClick={onClose}
        >
          {t("permanent.close")}
        </button>
      </div>
      {context.mode === "attackers" && (
        <CountPickerControls
          count={selectedAttackerCount}
          max={context.eligibleIds.length}
          onChange={selectAttackerCount}
        />
      )}
      {context.mode === "tap" && (
        <CountPickerControls
          count={Math.min(selectedTapCount, tapLimit)}
          max={tapLimit}
          onChange={selectTapCount}
        />
      )}
      {context.mode === "boardChoice" && (
        <BoardChoiceGroupControls
          choice={context.choice}
          eligibleIds={context.eligibleIds}
          groupIds={group.ids}
          objects={objects}
          selectedCardIds={selectedCardIds}
          setGroupSelectedCards={setGroupSelectedCards}
          onClose={onClose}
        />
      )}
      {context.mode === "blockTargets" && waitingFor?.type === "DeclareBlockers" && (
        <BlockTargetGroupControls
          eligibleIds={context.eligibleIds}
          blockerId={context.blockerId}
          waitingFor={waitingFor}
          combatAttackers={combatAttackers}
          blockerAssignments={blockerAssignments}
          setGroupBlockerAssignments={setGroupBlockerAssignments}
          objects={objects}
        />
      )}
      {context.mode === "blockers" && (
        <ObjectChoiceList
          eligibleIds={context.eligibleIds}
          onChoose={(id) => {
            combatClickHandler?.(id);
            onClose();
          }}
        />
      )}
      {context.mode === "equip" && waitingFor?.type === "EquipTarget" && (
        <ObjectChoiceList
          eligibleIds={context.eligibleIds}
          onChoose={(id) => {
            dispatchAction({
              type: "Equip",
              data: {
                equipment_id: waitingFor.data.equipment_id,
                target_id: id,
              },
            });
            onClose();
          }}
        />
      )}
      {context.mode === "target" && (
        <ObjectChoiceList
          eligibleIds={context.eligibleIds}
          onChoose={(id) => {
            dispatchAction({ type: "ChooseTarget", data: { target: { Object: id } } });
            onClose();
          }}
        />
      )}
    </div>,
    document.body,
  );
}

interface BoardChoiceGroupControlsProps {
  choice: BoardChoiceView;
  eligibleIds: ObjectId[];
  groupIds: ObjectId[];
  objects: Record<ObjectId, GameObject> | undefined;
  selectedCardIds: ObjectId[];
  setGroupSelectedCards: (groupIds: ObjectId[], selectedIds: ObjectId[]) => void;
  onClose: () => void;
}

function BoardChoiceGroupControls({
  choice,
  eligibleIds,
  groupIds,
  objects,
  selectedCardIds,
  setGroupSelectedCards,
  onClose,
}: BoardChoiceGroupControlsProps) {
  const { t } = useTranslation("game");
  const selectedForChoice = selectedCardIds.filter((id) => choice.objectIds.includes(id));
  const selectedInGroup = eligibleIds.filter((id) => selectedCardIds.includes(id));
  const maxSelection = boardChoiceMaxSelection(choice);

  // Every eligible id in a collapsed group is visually identical to the others
  // (same name, P/T, counters, keywords, tap/flip state — that's why they're
  // stacked). Distinguishing them with a #1..#N list (the old UI) forced the
  // player to pick among indistinguishable tokens, e.g. choosing which of five
  // Food tokens to sacrifice. Resolve by quantity instead.

  // Immediate single pick: nothing to distinguish — resolve with one click on a
  // labelled action button using the first eligible id.
  if (isBoardChoiceImmediate(choice)) {
    // Guard against an empty eligible list: the picker only opens when there is
    // at least one eligible id, but defending here avoids ever dispatching an
    // action with an undefined id payload.
    const firstId = eligibleIds[0];
    if (firstId === undefined) return null;
    return (
      <button
        type="button"
        className="w-full rounded bg-sky-700 px-2 py-1.5 font-bold text-white hover:bg-sky-600"
        onClick={() => {
          dispatchAction(buildBoardChoiceAction(choice, [firstId]));
          onClose();
        }}
      >
        {t(`boardChoice.actions.${choice.intent}`)}
      </button>
    );
  }

  // Multi-select: a quantity stepper that maps to the first N eligible ids.
  // The cap accounts for selections already made in other groups so the
  // choice-wide count limit can't be exceeded.
  const selectedOutsideGroup = Math.max(
    0,
    selectedForChoice.length - selectedInGroup.length,
  );
  const groupCeiling =
    maxSelection == null ? eligibleIds.length : Math.max(0, maxSelection - selectedOutsideGroup);
  const effectiveMax = Math.min(eligibleIds.length, groupCeiling);

  const setCount = (n: number) => {
    const clamped = Math.max(0, Math.min(n, effectiveMax));
    setGroupSelectedCards(groupIds, eligibleIds.slice(0, clamped));
  };

  const canConfirm = canConfirmBoardChoice(choice, selectedForChoice, objects);
  const requiredPower =
    choice.selection.type === "totalPowerAtLeast" ||
    choice.selection.type === "totalPowerAtMost"
      ? choice.selection.power
      : null;
  const power =
    requiredPower != null
      ? boardChoiceSelectedPower(choice, selectedForChoice, objects)
      : null;

  return (
    <div className="space-y-2">
      <CountPickerControls
        count={selectedInGroup.length}
        max={effectiveMax}
        onChange={setCount}
      />
      <div className="text-center text-[11px] text-slate-300">
        {power == null
          ? t("boardChoice.groupCount", {
              selected: selectedForChoice.length,
              count: maxSelection ?? eligibleIds.length,
            })
          : t("boardChoice.groupPower", {
              selected: power,
              required: requiredPower,
            })}
      </div>
      <button
        type="button"
        className="w-full rounded bg-sky-700 px-2 py-1 font-bold text-white disabled:cursor-not-allowed disabled:bg-slate-800 disabled:text-white/50"
        disabled={!canConfirm}
        onClick={() => {
          dispatchAction(buildBoardChoiceAction(choice, selectedForChoice));
          onClose();
        }}
      >
        {t("boardChoice.confirm")}
      </button>
    </div>
  );
}

interface BlockTargetGroupControlsProps {
  eligibleIds: ObjectId[];
  blockerId: ObjectId;
  waitingFor: Extract<WaitingFor, { type: "DeclareBlockers" }>;
  combatAttackers: AttackerInfo[] | undefined;
  blockerAssignments: BlockerAssignments;
  setGroupBlockerAssignments: (blockerId: ObjectId, groupIds: ObjectId[], attackerIds: ObjectId[]) => void;
  objects: Record<ObjectId, GameObject> | undefined;
}

/**
 * One `role="group"` per {@link BlockTargetStack} (see the axis citations on
 * `utils/combat.ts::partitionBlockTargets`), each with a count stepper that
 * assigns the pending blocker to that many of the stack's members. Not
 * `ObjectChoiceList`: every member of a stack is interchangeable by
 * construction, so a #1..#N list would force a choice among indistinguishable
 * tokens the way `BoardChoiceGroupControls` already avoids for board choices.
 */
function BlockTargetGroupControls({
  eligibleIds,
  blockerId,
  waitingFor,
  combatAttackers,
  blockerAssignments,
  setGroupBlockerAssignments,
  objects,
}: BlockTargetGroupControlsProps) {
  const { t } = useTranslation("game");

  const stacks = useMemo(
    () =>
      partitionBlockTargets(eligibleIds, blockerId, {
        attackers: combatAttackers,
        blockerAssignments,
        blockRequirements: waitingFor.data.block_requirements,
        blockerConstraints: waitingFor.data.blocker_constraints,
        mustBeBlockedTargets: waitingFor.data.must_be_blocked_targets,
        blockCapacities: waitingFor.data.block_capacities,
      }),
    [eligibleIds, blockerId, combatAttackers, blockerAssignments, waitingFor],
  );

  // CR 702.22c: label bands by a stable 1-based ordinal over the distinct
  // band ids across every attacker in the current combat (ascending) — not
  // just this picker's own stacks — so a band keeps the same number across
  // different pending blockers and different piles instead of renumbering
  // per picker.
  const bandOrdinals = useMemo(() => {
    const distinctBandIds = Array.from(
      new Set(
        (combatAttackers ?? [])
          .map((attacker) => attacker.band_id ?? null)
          .filter((id): id is number => id !== null),
      ),
    ).sort((a, b) => a - b);
    return new Map(distinctBandIds.map((id, index) => [id, index + 1]));
  }, [combatAttackers]);

  const blockerName = objects?.[blockerId]?.name ?? t("attackTargetPicker.objectFallback", { id: blockerId });

  return (
    <div className="space-y-2">
      <div className="truncate text-center text-[11px] font-semibold text-slate-200">
        {t("permanent.blockingWith", { name: blockerName })}
      </div>
      {stacks.map((stack) => {
        const targetName =
          objects?.[stack.attackTarget.data]?.name
          ?? t("attackTargetPicker.objectFallback", { id: stack.attackTarget.data });
        // Every candidate comes from `valid_block_targets[pendingBlocker]`,
        // which the picker only opens for the local player's own prompt
        // (`GroupedPermanentDisplay`'s `pickerContext` gate) — so a Player
        // target here is always this defender. No other-player name fallback.
        const targetLabel =
          stack.attackTarget.type === "Player"
            ? t("attackTargetPicker.you")
            : stack.attackTarget.type === "Planeswalker"
              ? t("attackTargetPicker.planeswalkerTarget", { name: targetName })
              : t("attackTargetPicker.battleTarget", { name: targetName });

        const labelParts = [targetLabel];
        if (stack.bandId !== null) {
          labelParts.push(t("combat.bandBadge", { n: bandOrdinals.get(stack.bandId) }));
        }
        if (stack.mustBlock) labelParts.push(t("combat.mustBlockBadge"));
        if (stack.mustBeBlocked) labelParts.push(t("combat.mustBeBlockedBadge"));
        if (stack.minBlockers > 0) {
          labelParts.push(t("combat.blockNeedsBadge", { required: stack.minBlockers }));
        }
        if (stack.otherBlockerIds.length > 0) {
          labelParts.push(
            t("permanent.blockedBy", {
              names: stack.otherBlockerIds
                .map((id) => objects?.[id]?.name ?? t("attackTargetPicker.objectFallback", { id }))
                .join(", "),
            }),
          );
        }
        // Without a band, no direct selection means this attacker is
        // unblocked. A banded attacker may be blocked through a band-mate,
        // which this direct-selection picker deliberately does not derive.
        if (
          stack.bandId === null &&
          stack.assignedIds.length === 0 &&
          stack.otherBlockerIds.length === 0
        ) {
          labelParts.push(t("permanent.unblocked"));
        }
        const label = labelParts.join(" · ");

        return (
          <div key={stack.key} role="group" aria-label={label} className="space-y-1">
            <div className="truncate text-[10px] text-slate-300">{label}</div>
            <CountPickerControls
              count={stack.assignedIds.length}
              max={stack.maxAssignable}
              onChange={(n) =>
                setGroupBlockerAssignments(blockerId, stack.ids, blockTargetSelection(stack, n))
              }
            />
          </div>
        );
      })}
    </div>
  );
}

interface CountPickerControlsProps {
  count: number;
  max: number;
  onChange: (count: number) => void;
}

function CountPickerControls({ count, max, onChange }: CountPickerControlsProps) {
  const { t } = useTranslation("game");
  return (
    <div className="grid grid-cols-4 gap-1">
      <button
        type="button"
        className="rounded bg-slate-800 px-2 py-1 font-bold disabled:opacity-40"
        disabled={count <= 0}
        onClick={() => onChange(count - 1)}
      >
        -1
      </button>
      <button
        type="button"
        className="rounded bg-slate-800 px-2 py-1 font-bold disabled:opacity-40"
        disabled={count >= max}
        onClick={() => onChange(count + 1)}
      >
        +1
      </button>
      <button
        type="button"
        className="rounded bg-slate-800 px-2 py-1 font-bold disabled:opacity-40"
        disabled={count >= max}
        onClick={() => onChange(max)}
      >
        {t("permanent.pickAll")}
      </button>
      <button
        type="button"
        className="rounded bg-slate-800 px-2 py-1 font-bold disabled:opacity-40"
        disabled={count <= 0}
        onClick={() => onChange(0)}
      >
        {t("permanent.pickNone")}
      </button>
      <div className="col-span-4 text-center text-[11px] text-slate-300">
        {count} / {max}
      </div>
    </div>
  );
}

interface ObjectChoiceListProps {
  eligibleIds: ObjectId[];
  onChoose: (id: ObjectId) => void;
}

function ObjectChoiceList({ eligibleIds, onChoose }: ObjectChoiceListProps) {
  return (
    <div className="grid max-h-48 grid-cols-2 gap-1 overflow-auto">
      {eligibleIds.map((id, index) => (
        <button
          key={id}
          type="button"
          className="rounded bg-slate-800 px-2 py-1 font-semibold hover:bg-slate-700"
          onClick={() => onChoose(id)}
        >
          #{index + 1}
        </button>
      ))}
    </div>
  );
}
