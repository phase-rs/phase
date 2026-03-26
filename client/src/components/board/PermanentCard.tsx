import { motion } from "framer-motion";
import { memo, useCallback, useMemo, useRef } from "react";
import { useShallow } from "zustand/react/shallow";

import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { ArtCropCard } from "../card/ArtCropCard.tsx";
import { CardImage } from "../card/CardImage.tsx";
import { PTBox } from "./PTBox.tsx";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { COUNTER_COLORS, computePTDisplay, formatCounterTooltip, formatCounterType, toRoman } from "../../viewmodel/cardProps.ts";
import { getCardDisplayColors } from "../card/cardFrame.ts";
import { KeywordStrip } from "./KeywordStrip.tsx";

interface PermanentCardProps {
  objectId: number;
}

const ATTACHMENT_OFFSET_PX = 15;
const EXILE_GHOST_OFFSET_PX = 20;

export const PermanentCard = memo(function PermanentCard({ objectId }: PermanentCardProps) {
  const playerId = usePlayerId();
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  const battlefieldCardDisplay = usePreferencesStore((s) => s.battlefieldCardDisplay);
  const tapRotation = usePreferencesStore((s) => s.tapRotation);
  const showKeywordStrip = usePreferencesStore((s) => s.showKeywordStrip) ?? true;

  const {
    selectedObjectId, selectObject, hoverObject, inspectObject,
    combatMode, selectedAttackers, toggleAttacker,
    blockerAssignments, combatClickHandler,
  } = useUiStore(useShallow((s) => ({
    selectedObjectId: s.selectedObjectId,
    selectObject: s.selectObject,
    hoverObject: s.hoverObject,
    inspectObject: s.inspectObject,
    combatMode: s.combatMode,
    selectedAttackers: s.selectedAttackers,
    toggleAttacker: s.toggleAttacker,
    blockerAssignments: s.blockerAssignments,
    combatClickHandler: s.combatClickHandler,
  })));
  const combatAttackers = useGameStore(
    (s) => s.gameState?.combat?.attackers,
  );
  // Targeting state — boolean selectors avoid subscribing to the full waitingFor object
  const isValidTarget = useGameStore((s) => {
    const wf = s.waitingFor;
    if ((wf?.type === "TargetSelection" || wf?.type === "TriggerTargetSelection") && wf.data.player === playerId) {
      return wf.data.selection.current_legal_targets.some(
        (target) => "Object" in target && target.Object === objectId,
      );
    }
    if (wf?.type === "CopyTargetChoice" && wf.data.player === playerId) {
      return wf.data.valid_targets.includes(objectId);
    }
    return false;
  });
  const isValidAttacker = useGameStore((s) =>
    s.waitingFor?.type === "DeclareAttackers"
      ? (s.waitingFor.data.valid_attacker_ids ?? []).includes(objectId)
      : false,
  );

  // Check if this permanent has activatable non-mana abilities from legal actions.
  // Mana abilities are also in legalActions (for auto-pass awareness) but excluded
  // here since canTapForMana handles their highlight independently.
  const hasActivatableAbility = useGameStore((s) => {
    const wf = s.waitingFor;
    if (!wf || wf.type !== "Priority" || wf.data.player !== playerId) return false;
    const obj = s.gameState?.objects[objectId];
    return s.legalActions.some((a) =>
      a.type === "ActivateAbility" && a.data.source_id === objectId
      && obj?.abilities?.[a.data.ability_index]?.effect?.type !== "Mana",
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
    if (!o || o.tapped || o.controller !== playerId) return false;
    // CR 605.1b: Lands and non-land permanents with mana abilities can tap for mana.
    // CR 302.6: Non-land creatures with summoning sickness cannot activate tap abilities.
    return o.card_types.core_types.includes("Land")
      || (o.has_mana_ability && !o.has_summoning_sickness);
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

  const handleMouseEnter = useCallback(() => {
    hoverObject(objectId); inspectObject(objectId);
  }, [hoverObject, inspectObject, objectId]);

  const handleMouseLeave = useCallback(() => {
    hoverObject(null); inspectObject(null);
  }, [hoverObject, inspectObject]);

  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const { handlers: longPressHandlers, firedRef: longPressFired } = useLongPress(
    useCallback(() => {
      inspectObject(objectId);
      setPreviewSticky(true);
    }, [inspectObject, setPreviewSticky, objectId]),
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

  // Tap rotation: 17deg in MTGA mode, 90deg in classic mode
  const tapOpacity = tapRotation === "mtga" && obj.tapped && !isAttacking ? 0.85 : 1;
  const isRotatedFull = isAttacking || obj.tapped;

  // Attacker slide-forward: player creatures slide up, opponent creatures slide down
  const attackSlide = isAttacking ? (obj.controller === playerId ? -30 : 30) : 0;

  const handleClick = () => {
    if (longPressFired.current) { longPressFired.current = false; return; }
    if (combatMode === "attackers") {
      if (isValidAttacker) toggleAttacker(objectId);
    } else if (combatMode === "blockers" && combatClickHandler) {
      combatClickHandler(objectId);
    } else if (isValidTarget) {
      dispatchAction({ type: "ChooseTarget", data: { target: { Object: objectId } } });
    } else if (isActivatable) {
      const o = useGameStore.getState().gameState?.objects[objectId];
      // Filter out mana abilities from non-mana ability actions — mana abilities
      // are in legalActions for auto-pass awareness but handled by canTapForMana.
      const abilityActions = useGameStore.getState().legalActions.filter((a) =>
        a.type === "ActivateAbility" && a.data.source_id === objectId
        && o?.abilities?.[a.data.ability_index]?.effect?.type !== "Mana",
      );
      if (abilityActions.length === 0 && canTapForMana) {
        // No non-mana abilities — tap for mana directly
        if (o && !o.card_types.core_types.includes("Land") && o.mana_ability_index != null) {
          // Non-land mana source: dispatch ActivateAbility (handles tap + sacrifice etc.)
          dispatchAction({ type: "ActivateAbility", data: { source_id: objectId, ability_index: o.mana_ability_index } });
        } else {
          dispatchAction({ type: "TapLandForMana", data: { object_id: objectId } });
        }
      } else if (abilityActions.length === 1 && !canTapForMana) {
        dispatchAction(abilityActions[0]);
      } else {
        // Multiple abilities or both ability + mana — show choice modal
        const allActions = [...abilityActions];
        if (canTapForMana) {
          if (o && !o.card_types.core_types.includes("Land") && o.mana_ability_index != null) {
            allActions.push({ type: "ActivateAbility", data: { source_id: objectId, ability_index: o.mana_ability_index } });
          } else {
            allActions.push({ type: "TapLandForMana", data: { object_id: objectId } });
          }
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
      data-card-hover
      layoutId={`permanent-${objectId}`}
      className="relative inline-flex w-fit cursor-pointer rounded-lg self-start select-none"
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
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
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
          <div className="relative z-10 rounded-lg overflow-hidden">
            <CardImage cardName={obj.name} size="small" unimplementedMechanics={obj.unimplemented_mechanics} colors={displayColors} isToken={obj.card_id === 0} className={glowClass} />
            {/* Keyword strip overlay — inside the card image wrapper so absolute positioning works */}
            {showKeywordStrip && obj.keywords.length > 0 && !obj.face_down && (
              <KeywordStrip keywords={obj.keywords} baseKeywords={obj.base_keywords} />
            )}
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
                  title={formatCounterTooltip(type, count)}
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
});

interface ExileGhostCardProps {
  objectId: number;
  offset: number;
}

const ExileGhostCard = memo(function ExileGhostCard({ objectId, offset }: ExileGhostCardProps) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  const { handlers: hoverHandlers } = useCardHover(objectId);
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
      data-card-hover
      {...hoverHandlers}
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
});

