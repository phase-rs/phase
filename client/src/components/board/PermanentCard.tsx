import { motion } from "framer-motion";
import { memo, useCallback, useMemo, useRef } from "react";
import { useShallow } from "zustand/react/shallow";

import type { GameAction } from "../../adapter/types.ts";
import { cardImageLookup } from "../../services/cardImageLookup.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { ArtCropCard } from "../card/ArtCropCard.tsx";
import { CardImage } from "../card/CardImage.tsx";
import { AttachmentChipRow } from "./AttachmentChipRow.tsx";
import { PTBox } from "./PTBox.tsx";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { COUNTER_COLORS, computePTDisplay, formatCounterTooltip, formatCounterType, toRoman } from "../../viewmodel/cardProps.ts";
import { getCardDisplayColors } from "../card/cardFrame.ts";
import { useBoardInteractionState } from "./BoardInteractionContext.tsx";
import { KeywordStrip } from "./KeywordStrip.tsx";
import { collectObjectActions } from "../../viewmodel/cardActionChoice.ts";

interface PermanentCardProps {
  objectId: number;
}

const EXILE_GHOST_OFFSET_PX = 20;

export const PermanentCard = memo(function PermanentCard({ objectId }: PermanentCardProps) {
  const playerId = usePlayerId();
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  const battlefieldCardDisplay = usePreferencesStore((s) => s.battlefieldCardDisplay);
  const tapRotation = usePreferencesStore((s) => s.tapRotation);
  const isCompactHeight = useIsCompactHeight();
  const showKeywordStrip = usePreferencesStore((s) => s.showKeywordStrip) ?? true;
  const {
    activatableObjectIds,
    committedAttackerIds,
    incomingAttackerCounts,
    manaTappableObjectIds,
    selectableManaCostCreatureIds,
    undoableTapObjectIds,
    validAttackerIds,
    validTargetObjectIds,
  } = useBoardInteractionState();

  const {
    selectedObjectId, selectObject, hoverObject, inspectObject,
    combatMode, selectedAttackers, toggleAttacker,
    blockerAssignments, combatClickHandler, selectedCardIds, toggleSelectedCard,
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
    selectedCardIds: s.selectedCardIds,
    toggleSelectedCard: s.toggleSelectedCard,
  })));
  const isValidTarget = validTargetObjectIds.has(objectId);
  const isValidAttacker = validAttackerIds.has(objectId);
  const hasActivatableAbility = activatableObjectIds.has(objectId);
  const canTapForMana = manaTappableObjectIds.has(objectId);
  const isActivatable = hasActivatableAbility || canTapForMana;
  const tapCreatureCostChoice = useGameStore((s) =>
    (s.waitingFor?.type === "TapCreaturesForManaAbility" || s.waitingFor?.type === "TapCreaturesForSpellCost") && s.waitingFor.data.player === playerId
      ? s.waitingFor.data
      : null,
  );
  const isSelectableForManaCost = selectableManaCostCreatureIds.has(objectId);
  const isSelectedForManaCost = isSelectableForManaCost && selectedCardIds.includes(objectId);

  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const cardRef = useRef<HTMLDivElement | null>(null);

  // On compact-height (landscape phones), use a subtler 12° rotation:
  // 17° (MTGA) widens the card's bounding box by ~26px on a 70px-wide
  // creature, which crowds tightly-packed attacker rows. 12° widens by
  // ~18px while still clearly reading as rotated.
  const tapAngle = isCompactHeight ? 12 : tapRotation === "mtga" ? 17 : 90;

  const allExileLinks = useGameStore((s) => s.gameState?.exile_links);
  const exileLinks = useMemo(
    () => allExileLinks?.filter((l) => l.source_id === objectId) ?? [],
    [allExileLinks, objectId],
  );

  const isUndoableTap = undoableTapObjectIds.has(objectId);

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
  const { name: imgName, faceIndex: imgFace } = cardImageLookup(obj);
  const hasSummoningSickness = obj.has_summoning_sickness ?? false;

  const ptDisplay = computePTDisplay(obj);
  const isSelected = selectedObjectId === objectId;

  // Combat state — check both UI selection and committed combat state
  const isSelectingAttacker =
    combatMode === "attackers" && selectedAttackers.includes(objectId);
  const isCommittedAttacker = committedAttackerIds.has(objectId);
  const isAttacking = isSelectingAttacker || isCommittedAttacker;
  const isBlocking =
    combatMode === "blockers" && blockerAssignments.has(objectId);
  // Passive imposed state: how many creatures are attacking this permanent?
  // Nonzero means a Planeswalker / Battle target declaration points here.
  const incomingAttackerCount = incomingAttackerCounts.get(objectId) ?? 0;
  const isUnderAttack = incomingAttackerCount > 0;

  // Glow ring styles.
  // Priority tiers: (1) action I'm taking — attacking / blocking, (2) passive
  // imposed state — under attack, (3) affordances offered — mana cost selection,
  // valid target, activatable, tap undo, (4) idle selection.
  let glowClass = "";
  if (isAttacking) {
    glowClass =
      "ring-2 ring-orange-500 shadow-[0_0_12px_3px_rgba(249,115,22,0.7)]";
  } else if (isBlocking) {
    glowClass =
      "ring-2 ring-orange-500 shadow-[0_0_12px_3px_rgba(249,115,22,0.7)]";
  } else if (isUnderAttack) {
    glowClass =
      "ring-2 ring-red-500 shadow-[0_0_14px_4px_rgba(220,38,38,0.55)]";
  } else if (isSelectedForManaCost) {
    glowClass =
      "ring-2 ring-emerald-400 shadow-[0_0_14px_4px_rgba(52,211,153,0.55)]";
  } else if (isSelectableForManaCost) {
    glowClass =
      "ring-2 ring-emerald-300/70 shadow-[0_0_10px_3px_rgba(74,222,128,0.35)]";
  } else if (isValidTarget) {
    glowClass =
      "ring-2 ring-amber-400/60 shadow-[0_0_12px_3px_rgba(201,176,55,0.8)]";
  } else if (isActivatable) {
    glowClass =
      "ring-2 ring-cyan-400 shadow-[0_0_14px_4px_rgba(34,211,238,0.55)]";
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

  // Filter out loyalty counters — shown separately as the loyalty badge
  const counters = Object.entries(obj.counters).filter((entry): entry is [string, number] => entry[1] != null && entry[0] !== "loyalty");

  // Tap rotation: 17deg in MTGA mode (or compact-height), 90deg in classic mode
  const tapOpacity = (isCompactHeight || tapRotation === "mtga") && obj.tapped && !isAttacking ? 0.85 : 1;
  const isRotatedFull = isAttacking || obj.tapped;

  // Attacker slide-forward: player creatures slide up, opponent creatures slide down.
  // Reduced on compact-height where 30px would overflow the small creature row.
  const attackSlideMagnitude = isCompactHeight ? 12 : 30;
  const attackSlide = isAttacking ? (obj.controller === playerId ? -attackSlideMagnitude : attackSlideMagnitude) : 0;

  const handleClick = () => {
    if (longPressFired.current) { longPressFired.current = false; return; }
    // TapCreaturesForManaAbility is mid-cost resolution — check before combat mode
    // so clicks land even when DeclareAttackers combat mode is active.
    if (isSelectableForManaCost && tapCreatureCostChoice) {
      if (
        isSelectedForManaCost
        || selectedCardIds.length < tapCreatureCostChoice.count
      ) {
        toggleSelectedCard(objectId);
      }
    } else if (combatMode === "attackers") {
      if (isValidAttacker) toggleAttacker(objectId);
    } else if (combatMode === "blockers" && combatClickHandler) {
      combatClickHandler(objectId);
    } else if (isValidTarget) {
      dispatchAction({ type: "ChooseTarget", data: { target: { Object: objectId } } });
    } else if (isActivatable) {
      const o = useGameStore.getState().gameState?.objects[objectId];
      // Read the engine-provided action list for this permanent — the mapping
      // from GameAction variant to source permanent is owned by the engine
      // (GameAction::source_object), not reconstructed here. Partitioning by
      // effect type (Mana vs other) is a display concern: mana abilities route
      // through the mana-tap UI; everything else routes through the ability
      // choice modal or auto-dispatches.
      const objectActions = collectObjectActions(
        useGameStore.getState().legalActionsByObject,
        objectId,
      );
      const abilityActions: Array<Extract<GameAction, { type: "ActivateAbility" }>> = [];
      const manaActions: Array<Extract<GameAction, { type: "ActivateAbility" }>> = [];
      const keywordActions: GameAction[] = [];
      for (const action of objectActions) {
        if (action.type === "ActivateAbility") {
          const effectType = o?.abilities?.[action.data.ability_index]?.effect?.type;
          if (effectType === "Mana") {
            manaActions.push(action);
          } else {
            abilityActions.push(action);
          }
        } else {
          // CR 113.3b keyword activations (Crew/Station/Equip/Saddle) and any
          // future per-permanent action are surfaced alongside activated
          // abilities in the choice modal.
          keywordActions.push(action);
        }
      }
      // Prefer ActivateAbility entries emitted by legal_actions — they carry the
      // exact ability_index and let the engine prompt ChooseManaColor for
      // multi-option productions (AnyOneColor, ChoiceAmongCombinations). The
      // TapLandForMana shortcut is only used when the engine emitted no mana
      // action (single-option basic lands pre-legalActions, edge cases).
      const manaFallback: GameAction = manaActions.length === 0
        ? { type: "TapLandForMana", data: { object_id: objectId } }
        : manaActions[0];
      const manaChoiceNeeded = manaActions.length > 1;

      const nonManaActions: GameAction[] = [...abilityActions, ...keywordActions];
      if (nonManaActions.length === 0 && canTapForMana) {
        if (manaChoiceNeeded) {
          setPendingAbilityChoice({ objectId, actions: manaActions });
        } else {
          dispatchAction(manaFallback);
        }
      } else if (nonManaActions.length === 1 && !canTapForMana) {
        dispatchAction(nonManaActions[0]);
      } else {
        const allActions: GameAction[] = [...nonManaActions];
        if (canTapForMana) {
          if (manaChoiceNeeded) {
            allActions.push(...manaActions);
          } else {
            allActions.push(manaFallback);
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
      className="relative inline-flex w-fit cursor-pointer rounded-lg self-end select-none"
      style={{
        zIndex: isAttacking ? 50 : undefined,
        filter: sicknessFilter,
        boxShadow: sicknessGlow,
        transformOrigin: "center center",
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
          <div className={`relative z-10 rounded-lg overflow-hidden ${glowClass}`}>
            <CardImage cardName={imgName} faceIndex={imgFace} size="small" unimplementedMechanics={obj.unimplemented_mechanics} colors={displayColors} isToken={obj.card_id === 0} tokenFilters={obj.card_id === 0 ? { power: obj.power, toughness: obj.toughness, colors: obj.color } : undefined} />
            {/* Keyword strip overlay — inside the card image wrapper so absolute positioning works */}
            {showKeywordStrip && obj.keywords.length > 0 && !obj.face_down && (
              <KeywordStrip keywords={obj.keywords} baseKeywords={obj.base_keywords} />
            )}
          </div>

          {/* P/T box for creatures */}
          {ptDisplay && <PTBox ptDisplay={ptDisplay} />}

          {/* Attached Equipment / Auras / etc. summarized as chips along the
              bottom edge of the host. Replaces the prior 15px top-peek of
              tucked cards. Sibling of PTBox so it inherits tap rotation. */}
          {obj.attachments.length > 0 && <AttachmentChipRow objectIds={obj.attachments} />}

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

          {/* Under-attack badge — ⚔×N in top-left. A single attacker shows
              just ⚔ (the ring carries the count of 1 well enough); multiple
              attackers show the count so gang-attack lethality is parseable
              at a glance. */}
          {isUnderAttack && (
            <div
              className="absolute left-1 top-1 z-20 flex items-center gap-0.5 rounded bg-red-700/85 px-1 py-0.5 text-[10px] font-bold text-white shadow"
              title={`Attacked by ${incomingAttackerCount} creature${incomingAttackerCount === 1 ? "" : "s"}`}
            >
              <span aria-hidden>⚔</span>
              {incomingAttackerCount > 1 && <span>×{incomingAttackerCount}</span>}
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
  const { name: imgName, faceIndex: imgFace } = cardImageLookup(obj);
  const useArtCrop = battlefieldCardDisplay === "art_crop";

  return (
    <div
      className="absolute z-0 cursor-default opacity-70"
      style={{ bottom: `-${offset}px`, left: `${offset}px` }}
      {...hoverHandlers}
    >
      {/* Purple exile tint */}
      <div className="absolute inset-0 z-10 rounded-lg bg-purple-600/30 pointer-events-none" />
      {useArtCrop ? (
        <ArtCropCard objectId={objectId} />
      ) : (
        <CardImage cardName={imgName} faceIndex={imgFace} size="small" colors={displayColors} isToken={obj.card_id === 0} tokenFilters={obj.card_id === 0 ? { power: obj.power, toughness: obj.toughness, colors: obj.color } : undefined} />
      )}
    </div>
  );
});
