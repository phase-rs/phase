import { motion } from "framer-motion";
import { useCallback, useMemo, useRef } from "react";

import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { ArtCropCard } from "../card/ArtCropCard.tsx";
import { CardImage } from "../card/CardImage.tsx";
import { PTBox } from "./PTBox.tsx";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { computePTDisplay, toRoman } from "../../viewmodel/cardProps.ts";
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
  const selectObject = useUiStore((s) => s.selectObject);
  const hoverObject = useUiStore((s) => s.hoverObject);
  const inspectObject = useUiStore((s) => s.inspectObject);
  const combatMode = useUiStore((s) => s.combatMode);
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const toggleAttacker = useUiStore((s) => s.toggleAttacker);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const combatClickHandler = useUiStore((s) => s.combatClickHandler);
  const combatAttackers = useGameStore(
    (s) => s.gameState?.combat?.attackers,
  );
  const waitingFor = useGameStore((s) => s.waitingFor);

  // Check if this permanent has activatable non-mana abilities from legal actions
  const hasActivatableAbility = useGameStore((s) => {
    const wf = s.waitingFor;
    if (!wf || wf.type !== "Priority" || wf.data.player !== playerId) return false;
    return s.legalActions.some((a) =>
      a.type === "ActivateAbility" && a.data.source_id === objectId,
    );
  });

  // Land tappability derived from game state — no need for legal_actions
  const canTapForMana = useGameStore((s) => {
    const wf = s.waitingFor;
    if (!wf) return false;
    // Mana abilities are legal during priority and mana payment
    const isPlayerActing =
      (wf.type === "Priority" && wf.data.player === playerId) ||
      (wf.type === "ManaPayment" && wf.data.player === playerId) ||
      (wf.type === "UnlessPayment" && wf.data.player === playerId);
    if (!isPlayerActing) return false;
    const o = s.gameState?.objects[objectId];
    return !!o && !o.tapped && o.controller === playerId
      && o.card_types.core_types.includes("Land");
  });
  const isActivatable = hasActivatableAbility || canTapForMana;

  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const cardRef = useRef<HTMLDivElement | null>(null);

  const tapAngle = tapRotation === "mtga" ? 17 : 90;

  const allExileLinks = useGameStore((s) => s.gameState?.exile_links);
  const exileLinks = useMemo(
    () => allExileLinks?.filter((l) => l.source_id === objectId) ?? [],
    [allExileLinks, objectId],
  );

  // Engine-driven undo check: land is in the player's lands_tapped_for_mana tracking
  const isUndoableTap = useGameStore((s) => {
    const tapped = s.gameState?.lands_tapped_for_mana?.[playerId] ?? [];
    return tapped.includes(objectId);
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
  const currentTargetRefs =
    waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection"
      ? waitingFor.data.selection.current_legal_targets
      : [];
  const isHumanTargetSelection =
    (waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection")
    && waitingFor.data.player === playerId;
  const isCopyTargetChoice =
    waitingFor?.type === "CopyTargetChoice" && waitingFor.data.player === playerId;
  const isValidTarget = (isHumanTargetSelection && currentTargetRefs.some(
    (target) => "Object" in target && target.Object === objectId,
  )) || (isCopyTargetChoice && waitingFor.data.valid_targets.includes(objectId));

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

  // Filter out Loyalty counters — shown separately as the loyalty badge
  const counters = Object.entries(obj.counters).filter(([type]) => type !== "Loyalty");

  const validAttackerIds =
    waitingFor?.type === "DeclareAttackers"
      ? (waitingFor.data.valid_attacker_ids ?? [])
      : [];

  // Tap rotation: 17deg in MTGA mode, 90deg in classic mode
  const tapOpacity = tapRotation === "mtga" && obj.tapped && !isAttacking ? 0.85 : 1;
  const isRotatedFull = isAttacking || obj.tapped;

  // Attacker slide-forward: player creatures slide up, opponent creatures slide down
  const attackSlide = isAttacking ? (obj.controller === playerId ? -30 : 30) : 0;

  const handleClick = () => {
    if (combatMode === "attackers") {
      if (validAttackerIds.includes(objectId)) toggleAttacker(objectId);
    } else if (combatMode === "blockers" && combatClickHandler) {
      combatClickHandler(objectId);
    } else if (isValidTarget) {
      dispatchAction({ type: "ChooseTarget", data: { target: { Object: objectId } } });
    } else if (isActivatable) {
      const abilityActions = useGameStore.getState().legalActions.filter((a) =>
        a.type === "ActivateAbility" && a.data.source_id === objectId,
      );
      if (abilityActions.length === 0 && canTapForMana) {
        // No non-mana abilities — tap for mana directly
        dispatchAction({ type: "TapLandForMana", data: { object_id: objectId } });
      } else if (abilityActions.length === 1 && !canTapForMana) {
        dispatchAction(abilityActions[0]);
      } else {
        // Multiple abilities or both ability + mana — show choice modal
        const allActions = [...abilityActions];
        if (canTapForMana) {
          allActions.push({ type: "TapLandForMana", data: { object_id: objectId } });
        }
        if (allActions.length === 1) {
          dispatchAction(allActions[0]);
        } else {
          setPendingAbilityChoice({ objectId, actions: allActions });
        }
      }
    } else if (isUndoableTap) {
      dispatchAction({ type: "UntapLandForMana", data: { object_id: objectId } });
    } else {
      selectObject(isSelected ? null : objectId);
    }
  };

  const useArtCrop = battlefieldCardDisplay === "art_crop";

  return (
    <motion.div
      ref={cardRef}
      data-object-id={objectId}
      layoutId={`permanent-${objectId}`}
      className="relative inline-flex w-fit cursor-pointer rounded-lg self-start"
      originX={0.5}
      originY={0.5}
      style={{
        filter: sicknessFilter,
        boxShadow: sicknessGlow,
        transformOrigin: "center center",
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
        rotate: isRotatedFull ? tapAngle : 0,
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

          {/* Class level badge (CR 716) — gold-leaf bookmark */}
          {obj.class_level != null && (
            <div className="absolute -bottom-[3px] -left-[3px] z-20">
              <div className="rounded-t-[3px] rounded-b-none bg-gradient-to-b from-amber-950 to-stone-900 px-1.5 pt-[3px] pb-[5px] border border-amber-800/60 shadow-md clip-bookmark">
                <span className="font-serif text-[10px] font-bold text-amber-300 drop-shadow-[0_1px_1px_rgba(0,0,0,0.8)]">
                  {toRoman(obj.class_level)}
                </span>
              </div>
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
