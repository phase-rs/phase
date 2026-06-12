import { memo, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type { ObjectId, WaitingFor } from "../../adapter/types.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { GroupedPermanent as GroupedPermanentType } from "../../viewmodel/battlefieldProps";
import { getWaitingForObjectChoiceIds } from "../../viewmodel/gameStateView.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useBoardInteractionState } from "./BoardInteractionContext.tsx";
import { PermanentCard } from "./PermanentCard.tsx";
import {
  getGroupRenderMode,
  groupStaggerPx,
  type BattlefieldRowType,
} from "./groupRenderMode.ts";

interface GroupedPermanentProps {
  group: GroupedPermanentType;
  rowType: BattlefieldRowType;
  manualExpanded: boolean;
  onExpand: () => void;
}

type PickerMode = "attackers" | "blockers" | "equip" | "target" | "tap";

interface PickerContext {
  mode: PickerMode;
  eligibleIds: ObjectId[];
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
      return waitingFor.data.player;
    default:
      return null;
  }
}

export const GroupedPermanentDisplay = memo(function GroupedPermanentDisplay({
  group,
  rowType,
  manualExpanded,
  onExpand,
}: GroupedPermanentProps) {
  const { t } = useTranslation("game");
  const [pickerOpen, setPickerOpen] = useState(false);
  const playerId = usePlayerId();
  const battlefieldCardDisplay = usePreferencesStore((s) => s.battlefieldCardDisplay);
  const combatMode = useUiStore((s) => s.combatMode);
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const setGroupSelectedAttackers = useUiStore((s) => s.setGroupSelectedAttackers);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const combatClickHandler = useUiStore((s) => s.combatClickHandler);
  const selectedCardIds = useUiStore((s) => s.selectedCardIds);
  const setGroupSelectedCards = useUiStore((s) => s.setGroupSelectedCards);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const {
    committedAttackerIds,
    validAttackerIds,
    validTargetObjectIds,
  } = useBoardInteractionState();
  const containsAttacker = useMemo(() => {
    if (rowType !== "creatures" || combatMode !== "blockers") return false;
    return group.ids.some((id) => committedAttackerIds.has(id));
  }, [combatMode, committedAttackerIds, group.ids, rowType]);

  const renderMode = getGroupRenderMode(group, {
    manualExpanded,
    containsCommittedAttackerDuringBlockers: containsAttacker,
  });

  const pickerContext = useMemo<PickerContext | null>(() => {
    if (renderMode !== "collapsed") return null;
    if (waitingForPlayer(waitingFor) !== playerId) return null;

    if (combatMode === "attackers") {
      const eligibleIds = group.ids.filter((id) => validAttackerIds.has(id));
      return eligibleIds.length > 0 ? { mode: "attackers", eligibleIds } : null;
    }

    if (combatMode === "blockers" && waitingFor?.type === "DeclareBlockers" && combatClickHandler) {
      const validBlockerIds = new Set(waitingFor.data.valid_blocker_ids);
      const eligibleIds = group.ids.filter((id) =>
        validBlockerIds.has(id)
        && !blockerAssignments.has(id)
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
    combatClickHandler,
    combatMode,
    group.ids,
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
          {group.count}
        </button>
      </div>
    );
  }

  if (renderMode === "collapsed") {
    return (
      <div className="relative">
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
          ×{group.count}
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
          committedAttackerCount={committedAttackerCount}
          eligibleCount={pickerContext?.eligibleIds.length ?? 0}
          selectedAttackerCount={selectedAttackerCount}
          selectedTapCount={selectedTapCount}
        />
        {pickerOpen && pickerContext && (
          <CollapsedGroupPicker
            context={pickerContext}
            group={group}
            selectedAttackers={selectedAttackers}
            selectedCardIds={selectedCardIds}
            setGroupSelectedAttackers={setGroupSelectedAttackers}
            setGroupSelectedCards={setGroupSelectedCards}
            waitingFor={waitingFor}
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
            zIndex: i,
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
        {group.count}
      </button>
    </div>
  );
});

interface CollapsedGroupBadgesProps {
  assignedBlockerCount: number;
  committedAttackerCount: number;
  eligibleCount: number;
  selectedAttackerCount: number;
  selectedTapCount: number;
}

function CollapsedGroupBadges({
  assignedBlockerCount,
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
      {actionCount > 0 && (
        <span className="rounded bg-white px-1.5 py-0.5 text-[10px] font-bold leading-none text-black shadow">
          {t("permanent.selectedCount", { count: actionCount })}
        </span>
      )}
    </div>
  );
}

interface CollapsedGroupPickerProps {
  context: PickerContext;
  group: GroupedPermanentType;
  selectedAttackers: ObjectId[];
  selectedCardIds: ObjectId[];
  setGroupSelectedAttackers: (groupIds: ObjectId[], selectedIds: ObjectId[]) => void;
  setGroupSelectedCards: (groupIds: ObjectId[], selectedIds: ObjectId[]) => void;
  waitingFor: WaitingFor | null | undefined;
  combatClickHandler: ((id: ObjectId) => void) | null;
  onClose: () => void;
}

function CollapsedGroupPicker({
  context,
  group,
  selectedAttackers,
  selectedCardIds,
  setGroupSelectedAttackers,
  setGroupSelectedCards,
  waitingFor,
  combatClickHandler,
  onClose,
}: CollapsedGroupPickerProps) {
  const { t } = useTranslation("game");
  const selectedAttackerCount = context.eligibleIds.filter((id) => selectedAttackers.includes(id)).length;
  const selectedTapCount = context.eligibleIds.filter((id) => selectedCardIds.includes(id)).length;

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

  return (
    <div className="absolute left-1/2 top-full z-50 mt-2 w-52 -translate-x-1/2 rounded border border-slate-500 bg-slate-950/95 p-2 text-xs text-white shadow-2xl">
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
