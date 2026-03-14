import { motion } from "framer-motion";
import { useCallback, useMemo } from "react";

import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { ArtCropCard } from "../card/ArtCropCard.tsx";
import { CardImage } from "../card/CardImage.tsx";
import { PTBox } from "./PTBox.tsx";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { computePTDisplay } from "../../viewmodel/cardProps.ts";
import { getCardDisplayColors } from "../card/cardFrame.ts";

interface PermanentCardProps {
  objectId: number;
}

const COUNTER_COLORS: Record<string, string> = {
  Plus1Plus1: "bg-green-600",
  Minus1Minus1: "bg-red-600",
  Loyalty: "bg-amber-600",
};

const ATTACHMENT_OFFSET_PX = 15;
const EXILE_GHOST_OFFSET_PX = 20;

export function PermanentCard({ objectId }: PermanentCardProps) {
  const playerId = usePlayerId();
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  const battlefieldCardDisplay = usePreferencesStore((s) => s.battlefieldCardDisplay);
  const tapRotation = usePreferencesStore((s) => s.tapRotation);

  const selectedObjectId = useUiStore((s) => s.selectedObjectId);
  const targetingMode = useUiStore((s) => s.targetingMode);
  const selectObject = useUiStore((s) => s.selectObject);
  const clearTargets = useUiStore((s) => s.clearTargets);
  const hoverObject = useUiStore((s) => s.hoverObject);
  const inspectObject = useUiStore((s) => s.inspectObject);
  const combatMode = useUiStore((s) => s.combatMode);
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const toggleAttacker = useUiStore((s) => s.toggleAttacker);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const combatClickHandler = useUiStore((s) => s.combatClickHandler);
  const validTargetIds = useUiStore((s) => s.validTargetIds);
  const combatAttackers = useGameStore(
    (s) => s.gameState?.combat?.attackers,
  );
  const waitingFor = useGameStore((s) => s.waitingFor);
  const undo = useGameStore((s) => s.undo);

  // Primitive count — stable reference for glow ring, no infinite loop
  const activatableCount = useGameStore((s) => {
    const wf = s.waitingFor;
    if (!wf || wf.type !== "Priority" || wf.data.player !== playerId) return 0;
    return s.legalActions.filter((a) =>
      (a.type === "ActivateAbility" && a.data.source_id === objectId) ||
      (a.type === "TapLandForMana" && a.data.object_id === objectId),
    ).length;
  });
  const isActivatable = activatableCount > 0;

  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);

  const allExileLinks = useGameStore((s) => s.gameState?.exile_links);
  const exileLinks = useMemo(
    () => allExileLinks?.filter((l) => l.source_id === objectId) ?? [],
    [allExileLinks, objectId],
  );

  // Check if this specific permanent was tapped in the most recent undoable action
  // by comparing its tapped state against the previous snapshot
  const isUndoableTap = useGameStore((s) => {
    if (s.stateHistory.length === 0) return false;
    const prev = s.stateHistory[s.stateHistory.length - 1];
    const prevObj = prev.objects[objectId];
    const curObj = s.gameState?.objects[objectId];
    return !!prevObj && !prevObj.tapped && !!curObj?.tapped && curObj.controller === playerId;
  });

  const longPressHandlers = useLongPress(
    useCallback(() => {
      inspectObject(objectId);
    }, [inspectObject, objectId]),
  );

  if (!obj) return null;

  const isLand = obj.card_types.core_types.includes("Land");
  const displayColors = getCardDisplayColors(
    obj.color,
    isLand,
    obj.card_types.subtypes,
    obj.available_mana_colors,
  );
  const hasSummoningSickness = obj.has_summoning_sickness ?? false;

  const ptDisplay = computePTDisplay(obj);
  const isSelected = selectedObjectId === objectId;
  const isValidTarget = targetingMode && validTargetIds.includes(objectId);

  // Combat state — check both UI selection and committed combat state
  const isSelectingAttacker =
    combatMode === "attackers" && selectedAttackers.includes(objectId);
  const isCommittedAttacker =
    combatAttackers?.some((a) => a.object_id === objectId) ?? false;
  const isAttacking = isSelectingAttacker || isCommittedAttacker;
  const isBlocking =
    combatMode === "blockers" && blockerAssignments.has(objectId);

  // Glow ring styles (combat takes priority)
  let glowClass = "";
  if (isAttacking) {
    glowClass =
      "ring-2 ring-orange-500 shadow-[0_0_12px_3px_rgba(249,115,22,0.7)]";
  } else if (isBlocking) {
    glowClass =
      "ring-2 ring-orange-500 shadow-[0_0_12px_3px_rgba(249,115,22,0.7)]";
  } else if (isValidTarget) {
    glowClass =
      "ring-2 ring-amber-400/60 shadow-[0_0_12px_3px_rgba(201,176,55,0.8)]";
  } else if (isActivatable) {
    glowClass =
      "ring-2 ring-cyan-400/60 shadow-[0_0_10px_2px_rgba(34,211,238,0.3)]";
  } else if (isUndoableTap) {
    glowClass =
      "ring-1 ring-amber-400/40 shadow-[0_0_6px_1px_rgba(201,176,55,0.3)]";
  } else if (isSelected) {
    glowClass =
      "ring-2 ring-white shadow-[0_0_8px_2px_rgba(255,255,255,0.6)]";
  }

  const sicknessFilter = hasSummoningSickness ? "saturate(50%)" : undefined;
  const sicknessGlow = hasSummoningSickness
    ? "0 0 6px 1px rgba(255,255,255,0.3)"
    : undefined;

  const counters = Object.entries(obj.counters);

  const validAttackerIds =
    waitingFor?.type === "DeclareAttackers"
      ? (waitingFor.data.valid_attacker_ids ?? [])
      : [];

  // Tap rotation: 17deg in MTGA mode, 90deg in classic mode
  const tapAngle = tapRotation === "mtga" ? 17 : 90;
  const tapOpacity = tapRotation === "mtga" && obj.tapped && !isAttacking ? 0.85 : 1;

  // Attacker slide-forward: player creatures slide up, opponent creatures slide down
  const attackSlide = isAttacking ? (obj.controller === playerId ? -30 : 30) : 0;

  const handleClick = () => {
    if (combatMode === "attackers") {
      if (validAttackerIds.includes(objectId)) toggleAttacker(objectId);
    } else if (combatMode === "blockers" && combatClickHandler) {
      combatClickHandler(objectId);
    } else if (targetingMode && isValidTarget) {
      dispatchAction({ type: "SelectTargets", data: { targets: [{ Object: objectId }] } });
      clearTargets();
    } else if (isActivatable) {
      const actions = useGameStore.getState().legalActions.filter((a) =>
        (a.type === "ActivateAbility" && a.data.source_id === objectId) ||
        (a.type === "TapLandForMana" && a.data.object_id === objectId),
      );
      if (actions.length === 1) {
        dispatchAction(actions[0]);
      } else {
        setPendingAbilityChoice({ objectId, actions });
      }
    } else if (isUndoableTap) {
      undo();
    } else {
      selectObject(isSelected ? null : objectId);
    }
  };

  const useArtCrop = battlefieldCardDisplay === "art_crop";

  return (
    <motion.div
      data-object-id={objectId}
      layoutId={`permanent-${objectId}`}
      className="relative inline-flex w-fit cursor-pointer rounded-lg self-start"
      style={{
        filter: sicknessFilter,
        boxShadow: sicknessGlow,
        // Reserve space above for tucked attachments
        marginTop:
          obj.attachments.length > 0
            ? `${obj.attachments.length * ATTACHMENT_OFFSET_PX}px`
            : undefined,
        // Reserve space below for exile ghost cards
        marginBottom:
          exileLinks.length > 0
            ? `${exileLinks.length * EXILE_GHOST_OFFSET_PX}px`
            : undefined,
      }}
      animate={{
        rotate: isAttacking || obj.tapped ? tapAngle : 0,
        opacity: tapOpacity,
        y: attackSlide,
      }}
      transition={{ type: "spring", stiffness: 300, damping: 20 }}
      onClick={handleClick}
      onMouseEnter={() => { hoverObject(objectId); inspectObject(objectId); }}
      onMouseLeave={() => { hoverObject(null); inspectObject(null); }}
      {...longPressHandlers}
    >
      {/* Attachments rendered behind, tucked with top edge visible */}
      {obj.attachments.map((attachId, i) => (
        <div
          key={attachId}
          className="absolute left-0 z-0"
          style={{
            top: `${-(i + 1) * ATTACHMENT_OFFSET_PX}px`,
          }}
        >
          <PermanentCard objectId={attachId} />
        </div>
      ))}

      {/* Exile ghosts — cards held in exile by this permanent, peeking from below */}
      {exileLinks.map((link, i) => (
        <ExileGhostCard
          key={link.exiled_id}
          objectId={link.exiled_id}
          offset={(i + 1) * EXILE_GHOST_OFFSET_PX}
        />
      ))}

      {/* Main card — art crop or full card based on preference */}
      {useArtCrop ? (
        <div className={`relative z-10 rounded-lg ${glowClass}`}>
          <ArtCropCard objectId={objectId} />
        </div>
      ) : (
        <>
          <div className="relative z-10 rounded-lg">
            <CardImage cardName={obj.name} size="small" unimplementedMechanics={obj.unimplemented_mechanics} colors={displayColors} isToken={obj.card_id === 0} className={glowClass} />
          </div>

          {/* P/T box for creatures */}
          {ptDisplay && <PTBox ptDisplay={ptDisplay} />}

          {/* Damage overlay for non-creatures only (creatures use P/T box) */}
          {!ptDisplay && obj.damage_marked > 0 && (
            <div className="absolute inset-x-0 bottom-0 z-20 flex h-6 items-center justify-center rounded-b-lg bg-red-600/60 text-xs font-bold text-white">
              -{obj.damage_marked}
            </div>
          )}

          {/* Loyalty shield for planeswalkers */}
          {obj.loyalty != null && (
            <div className="absolute bottom-0 left-1/2 z-20 -translate-x-1/2 rounded-t bg-gray-900/90 px-1.5 py-0.5 text-xs font-bold text-amber-300">
              {obj.loyalty}
            </div>
          )}

          {/* Counter badges (top-right to avoid overlap with P/T box) */}
          {counters.length > 0 && (
            <div className="absolute right-1 top-1 z-20 flex flex-col gap-0.5">
              {counters.map(([type, count]) => (
                <span
                  key={type}
                  className={`rounded px-1 text-[10px] font-bold text-white ${COUNTER_COLORS[type] ?? "bg-purple-600"}`}
                >
                  {formatCounterType(type)} x{count}
                </span>
              ))}
            </div>
          )}
        </>
      )}

    </motion.div>
  );
}

interface ExileGhostCardProps {
  objectId: number;
  offset: number;
}

function ExileGhostCard({ objectId, offset }: ExileGhostCardProps) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  const inspectObject = useUiStore((s) => s.inspectObject);
  const battlefieldCardDisplay = usePreferencesStore((s) => s.battlefieldCardDisplay);

  if (!obj) return null;

  const isLand = obj.card_types.core_types.includes("Land");
  const displayColors = getCardDisplayColors(
    obj.color,
    isLand,
    obj.card_types.subtypes,
    obj.available_mana_colors,
  );
  const useArtCrop = battlefieldCardDisplay === "art_crop";

  return (
    <div
      className="absolute z-0 cursor-default opacity-70"
      style={{ bottom: `-${offset}px`, left: `${offset}px` }}
      onMouseEnter={() => inspectObject(objectId)}
      onMouseLeave={() => inspectObject(null)}
    >
      {/* Purple exile tint */}
      <div className="absolute inset-0 z-10 rounded-lg bg-purple-600/30 pointer-events-none" />
      {useArtCrop ? (
        <ArtCropCard objectId={objectId} />
      ) : (
        <CardImage cardName={obj.name} size="small" colors={displayColors} isToken={obj.card_id === 0} />
      )}
    </div>
  );
}

function formatCounterType(type: string): string {
  if (type === "Plus1Plus1") return "+1/+1";
  if (type === "Minus1Minus1") return "-1/-1";
  return type;
}
