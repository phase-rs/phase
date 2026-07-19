import { motion } from "framer-motion";
import type React from "react";
import { memo, useCallback, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";

import type { GameAction, GameObject, Keyword } from "../../adapter/types.ts";
import { cardImageLookup, tokenFiltersForObject } from "../../services/cardImageLookup.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { ArtCropCard } from "../card/ArtCropCard.tsx";
import { CardImage } from "../card/CardImage.tsx";
import { PTBox } from "./PTBox.tsx";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useCanHover } from "../../hooks/useCanHover.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useIsMobile } from "../../hooks/useIsMobile.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { buildGrantedKeywordSources, buildPTSources } from "../../viewmodel/attribution.ts";
import { COUNTER_COLORS, computePTDisplay, counterIconClass, formatCounterType, toRoman } from "../../viewmodel/cardProps.ts";
import { getCardDisplayColors } from "../card/cardFrame.ts";
import { ManaFontIcon } from "../icons/ManaFontIcon.tsx";
import { CounterTooltip } from "../ui/CounterTooltip.tsx";
import { LoyaltyBadge } from "../ui/LoyaltyBadge.tsx";
import { useBoardInteractionState } from "./BoardInteractionContext.tsx";
import { KeywordStrip } from "./KeywordStrip.tsx";
import {
  boardChoiceMaxSelection,
  buildBoardChoiceAction,
  getBoardChoiceView,
  isBoardChoiceImmediate,
  type BoardChoiceIntent,
} from "../../viewmodel/gameStateView.ts";
import {
  collectObjectActions,
  isManaObjectAction,
  resolveSingleActionDispatch,
} from "../../viewmodel/cardActionChoice.ts";

interface PermanentCardProps {
  objectId: number;
  attachmentsLiftedByAncestor?: boolean;
  attachmentRenderPath?: readonly number[];
  onPrimaryClickOverride?: () => void;
  /** When this card is the visible representative of a collapsed identical-permanent
   *  group (see GroupedPermanent collapsed mode), the full list of object ids it
   *  stands in for. Rendered as `data-grouped-ids` so DOM-driven animations
   *  (card slam, position lookup) can resolve a non-rendered swarm member to this
   *  visible card instead of silently no-op'ing. */
  coveredIds?: number[];
}

const EXILE_GHOST_OFFSET_PX = 20;
// Attachments stagger to the RIGHT of the host instead of above so the host
// row's vertical layout is unchanged — adding marginTop to reserve peek
// space made hosts uneven against their neighbors. The right side of a
// card naturally includes the mana-cost zone at top, which is where the
// subtype badge lives, so a rightward peek surfaces the type indicator
// without eating any of the host's frame.
//
// `BASE_PEEK_PX` is how much of the closest attachment sticks out past the
// host's right edge. Each subsequent attachment in the stack reveals a
// further `STACK_STEP_PX` so a creature with two Auras shows both visible
// portions cleanly without occluding either.
// 22px = badge size (20) + right-0.5 padding (2). Just enough for the
// AttachmentTypeBadge to be visible past the host's right edge with no
// extra card art revealed — the badge alone carries the "is this
// attached?" + "what type?" signal; the actual card is hover-accessible
// via the recursive PermanentCard's existing handlers.
const ATTACHMENT_PEEK_PX = 22;
const ATTACHMENT_STACK_STEP_PX = 22;
const HOVERED_CARD_Z_INDEX = 60;
const HOVERED_ATTACHMENT_HOST_Z_INDEX = 80;
const EMPTY_KEYWORD_BADGES: Keyword[] = [];

// Subtype glyphs sit in the top-right of the peek (where the mana pips
// would normally be) so the player can identify the attachment's role
// without parsing the title. Glyph palette matches the original chip
// design, intentionally disjoint from CardPreview's category icons so
// the badge can never be confused with a parsed-ability pill.
function attachmentTypeGlyph(subtypes: string[]): string | null {
  if (subtypes.includes("Equipment")) return "⚒";
  if (subtypes.includes("Aura")) return "✧";
  if (subtypes.includes("Fortification")) return "▣";
  return null;
}

function attachmentTreeContains(
  objects: Record<string, GameObject> | undefined,
  rootId: number,
  candidateId: number | null,
): boolean {
  if (candidateId == null) return false;
  const remaining = [rootId];
  const visited = new Set<number>();

  while (remaining.length > 0) {
    const id = remaining.pop();
    if (id == null || visited.has(id)) continue;
    if (id === candidateId) return true;

    visited.add(id);
    const current = objects?.[id];
    if (current) {
      remaining.push(...current.attachments);
    }
  }

  return false;
}

function objectIdFromRelatedTarget(target: EventTarget | null): number | null {
  if (!(target instanceof Element)) return null;
  const objectEl = target.closest<HTMLElement>("[data-object-id]");
  if (!objectEl) return null;
  const objectId = Number(objectEl.dataset.objectId);
  return Number.isFinite(objectId) ? objectId : null;
}

// Selected board-choice cards get a bright ring PLUS an inset fill so the whole
// card reads as "lit up / chosen" — a clearly stronger signal than the outline-
// only `availableBoardChoiceGlowClass` used for eligible-but-unselected cards.
// The inset differentiates selection independently of card art (blank/tokened
// cards otherwise looked identical selected vs. merely available).
function selectedBoardChoiceGlowClass(intent: BoardChoiceIntent): string {
  switch (intent) {
    case "sacrifice":
      return "ring-2 ring-red-400 shadow-[0_0_14px_4px_rgba(248,113,113,0.55),inset_0_0_18px_5px_rgba(248,113,113,0.3)]";
    case "tap":
      return "ring-2 ring-emerald-400 shadow-[0_0_14px_4px_rgba(52,211,153,0.55),inset_0_0_18px_5px_rgba(52,211,153,0.3)]";
    case "blight":
      return "ring-2 ring-purple-400 shadow-[0_0_14px_4px_rgba(192,132,252,0.55),inset_0_0_18px_5px_rgba(192,132,252,0.3)]";
    case "ringBearer":
      return "ring-2 ring-amber-300 shadow-[0_0_14px_4px_rgba(252,211,77,0.55),inset_0_0_18px_5px_rgba(252,211,77,0.3)]";
    case "return":
    case "exile":
    case "crew":
    case "saddle":
    case "station":
    case "keep":
      return "ring-2 ring-sky-300 shadow-[0_0_14px_4px_rgba(125,211,252,0.55),inset_0_0_18px_5px_rgba(125,211,252,0.3)]";
  }
}

function availableBoardChoiceGlowClass(intent: BoardChoiceIntent): string {
  switch (intent) {
    case "sacrifice":
      return "ring-2 ring-red-300/80 shadow-[0_0_10px_3px_rgba(248,113,113,0.35)]";
    case "tap":
      return "ring-2 ring-emerald-300/70 shadow-[0_0_10px_3px_rgba(74,222,128,0.35)]";
    case "blight":
      return "ring-2 ring-purple-300/80 shadow-[0_0_10px_3px_rgba(216,180,254,0.35)]";
    case "ringBearer":
      return "ring-2 ring-amber-300/80 shadow-[0_0_10px_3px_rgba(252,211,77,0.35)]";
    case "return":
    case "exile":
    case "crew":
    case "saddle":
    case "station":
    case "keep":
      return "ring-2 ring-sky-300/80 shadow-[0_0_10px_3px_rgba(125,211,252,0.35)]";
  }
}

function boardChoiceBadgeClass(intent: BoardChoiceIntent): string {
  switch (intent) {
    case "sacrifice":
      return "bg-red-500 text-white";
    case "tap":
      return "bg-emerald-500 text-emerald-950";
    case "blight":
      return "bg-purple-500 text-white";
    case "ringBearer":
      return "bg-amber-400 text-amber-950";
    case "return":
    case "exile":
    case "crew":
    case "saddle":
    case "station":
    case "keep":
      return "bg-sky-400 text-sky-950";
  }
}

export const PermanentCard = memo(function PermanentCard({
  objectId,
  attachmentsLiftedByAncestor = false,
  attachmentRenderPath = [],
  onPrimaryClickOverride,
  coveredIds,
}: PermanentCardProps) {
  const { t } = useTranslation("game");
  const isMobile = useIsMobile();
  const canHover = useCanHover();
  const playerId = usePlayerId();
  const gameObjects = useGameStore((s) => s.gameState?.objects);
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  const battlefieldKeywordBadges = useGameStore(
    (s) =>
      s.gameState?.derived?.battlefield_keyword_badges?.[String(objectId)]
      ?? EMPTY_KEYWORD_BADGES,
  );
  const isManaPaymentPreviewSource = useGameStore((s) =>
    s.manaPaymentPreviewSourceIds.includes(objectId),
  );
  const isRingBearer = useGameStore((s) => {
    const object = s.gameState?.objects[objectId];
    return object ? s.gameState?.ring_bearer?.[String(object.controller)] === objectId : false;
  });
  const battlefieldCardDisplay = usePreferencesStore((s) => s.battlefieldCardDisplay);
  const tapRotation = usePreferencesStore((s) => s.tapRotation);
  const isCompactHeight = useIsCompactHeight();
  const showKeywordStrip = usePreferencesStore((s) => s.showKeywordStrip) ?? true;
  // Narrow subscriptions so a non-attribution state change (mana pool, phase,
  // animation tick) doesn't re-render every PermanentCard on the board.
  const objectAttribution = useGameStore(
    (s) => s.gameState?.attribution?.[String(objectId)],
  );
  const transientContinuousEffects = useGameStore(
    (s) => s.gameState?.transient_continuous_effects,
  );
  const objId = obj?.id;
  const keywordSourceMap = useMemo(
    () =>
      objId !== undefined
        ? buildGrantedKeywordSources(objectAttribution, objId, {
            objects: gameObjects,
            transientContinuousEffects,
          })
        : undefined,
    [objectAttribution, transientContinuousEffects, gameObjects, objId],
  );
  const ptSources = useMemo(
    () =>
      objId !== undefined
        ? buildPTSources(objectAttribution, objId, {
            objects: gameObjects,
            transientContinuousEffects,
          })
        : undefined,
    [objectAttribution, transientContinuousEffects, gameObjects, objId],
  );
  const {
    activatableObjectIds,
    boardChoiceObjectIds,
    committedAttackerIds,
    incomingAttackerCounts,
    manaTappableObjectIds,
    selectableManaCostCreatureIds,
    selectableSacrificeObjectIds,
    undoableTapObjectIds,
    validAttackerIds,
    validTargetObjectIds,
  } = useBoardInteractionState();

  const selectedObjectId = useUiStore((s) => s.selectedObjectId);
  const selectObject = useUiStore((s) => s.selectObject);
  const hoverObject = useUiStore((s) => s.hoverObject);
  const inspectObject = useUiStore((s) => s.inspectObject);
  const debugHighlightedObjectId = useUiStore((s) => s.debugHighlightedObjectId);
  const combatMode = useUiStore((s) => s.combatMode);
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const toggleAttacker = useUiStore((s) => s.toggleAttacker);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const combatClickHandler = useUiStore((s) => s.combatClickHandler);
  const selectedCardIds = useUiStore((s) => s.selectedCardIds);
  const toggleSelectedCard = useUiStore((s) => s.toggleSelectedCard);
  // Hover is read as derived booleans, NOT the raw hoveredObjectId, so hovering
  // any permanent re-renders only the card whose hovered/lifted state actually
  // flips — not every PermanentCard on the board. O(1) per hover, not O(N).
  const isHovered = useUiStore((s) => s.hoveredObjectId === objectId);
  const isInspected = useUiStore((s) => s.inspectedObjectId === objectId);
  // Lifting a host's attachments only applies to cards that HAVE attachments;
  // for the common (unattached) card this selector is a constant `false`, so it
  // never re-renders on hover. Attached cards re-render only when their lifted
  // state changes. Mirrors the `obj.attachments.length > 0` gate below.
  const hasAttachments = (obj?.attachments.length ?? 0) > 0;
  const isInHoveredAttachmentTree = useUiStore((s) =>
    hasAttachments ? attachmentTreeContains(gameObjects, objectId, s.hoveredObjectId) : false,
  );
  // Debug-panel preview highlight: lights up only when the user is hovering
  // an ObjectSelect option (or otherwise dispatching `setDebugHighlightedObjectId`).
  // Deliberately distinct from the standard hover-lift so the debug signal
  // never blends into ambient interaction state.
  const isDebugHighlighted = debugHighlightedObjectId === objectId;
  const isValidTarget = validTargetObjectIds.has(objectId);
  const isValidAttacker = validAttackerIds.has(objectId);
  const hasActivatableAbility = activatableObjectIds.has(objectId);
  const canTapForMana = manaTappableObjectIds.has(objectId);
  const isActivatable = hasActivatableAbility || canTapForMana;
  const tapCreatureCostChoice = useGameStore((s) =>
    s.waitingFor?.type === "PayCost"
    && s.waitingFor.data.kind.type === "TapCreatures"
    && s.waitingFor.data.player === playerId
      ? s.waitingFor.data
      : null,
  );
  const waitingFor = useGameStore((s) => s.waitingFor);
  const boardChoice = useMemo(() => {
    const choice = getBoardChoiceView(waitingFor, gameObjects);
    return choice?.player === playerId ? choice : null;
  }, [gameObjects, playerId, waitingFor]);
  const equipTargetChoice = useGameStore((s) =>
    s.waitingFor?.type === "EquipTarget" && s.waitingFor.data.player === playerId
      ? s.waitingFor.data
      : null,
  );
  const isSelectableForManaCost = selectableManaCostCreatureIds.has(objectId);
  const isSelectedForManaCost = isSelectableForManaCost && selectedCardIds.includes(objectId);
  const isSelectableForBoardChoice = boardChoiceObjectIds.has(objectId) && boardChoice != null;
  const isSelectedForBoardChoice = isSelectableForBoardChoice && selectedCardIds.includes(objectId);
  const selectedBoardChoiceIds = boardChoice
    ? selectedCardIds.filter((id) => boardChoice.objectIds.includes(id))
    : [];

  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const setAttachmentFanHost = useUiStore((s) => s.setAttachmentFanHost);
  const dismissPreview = useUiStore((s) => s.dismissPreview);
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

  // On touch-only devices, skip mouse events — synthesized mouseenter from touch fires
  // inspectObject every touch, opening the full-screen MobilePreviewOverlay
  // and blocking combat interactions (blocker/attacker selection).
  const handleMouseEnter = useCallback(() => {
    if (isMobile || !canHover) return;
    hoverObject(objectId); inspectObject(objectId);
  }, [canHover, hoverObject, inspectObject, isMobile, objectId]);

  const handleMouseLeave = useCallback((event: React.MouseEvent<HTMLDivElement>) => {
    if (isMobile || !canHover) return;
    const nextObjectId = objectIdFromRelatedTarget(event.relatedTarget);
    hoverObject(nextObjectId);
    inspectObject(nextObjectId);
  }, [canHover, hoverObject, inspectObject, isMobile]);

  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const { handlers: longPressHandlers, firedRef: longPressFired } = useLongPress(
    useCallback(() => {
      inspectObject(objectId);
      setPreviewSticky(true);
    }, [inspectObject, setPreviewSticky, objectId]),
  );

  const controllerIdentity = useGameStore(
    (s) => obj && s.gameState?.players?.find((p) => p.id === obj.controller)?.commander_color_identity,
  );

  const openAttachmentFan = useCallback((event: React.MouseEvent<HTMLButtonElement>) => {
    event.stopPropagation();
    dismissPreview();
    setAttachmentFanHost(objectId);
  }, [dismissPreview, objectId, setAttachmentFanHost]);

  if (!obj) return null;

  const isLand = obj.card_types.core_types.includes("Land");
  const displayColors = getCardDisplayColors(
    obj.color,
    isLand,
    obj.card_types.subtypes,
    obj.available_mana_pips,
    controllerIdentity || undefined,
  );
  const { name: imgName, faceIndex: imgFace, oracleId: imgOracleId, faceName: imgFaceName } = cardImageLookup(obj);
  const hasSummoningSickness = obj.has_summoning_sickness ?? false;

  const ptDisplay = computePTDisplay(obj);
  const isSelected = selectedObjectId === objectId;
  // CR 301.5 / CR 303.4: An attached Equipment/Aura is an independent permanent
  // that can be a valid target, an activation source (re-equip), or a board
  // choice in its own right. Collapsed behind its host it is unreachable —
  // clicks land on the host instead, so a "put a counter on target nonland
  // permanent you control" trigger lands on the creature rather than the chosen
  // Equipment, and an attached Equipment can't be re-activated to move it. Open
  // a host's attachments whenever any of them is actionable in the current
  // waiting state so each is independently clickable without requiring a hover.
  const attachmentsActionable =
    obj.attachments.length > 0
    && obj.attachments.some(
      (id) =>
        validTargetObjectIds.has(id)
        || activatableObjectIds.has(id)
        || manaTappableObjectIds.has(id)
        || boardChoiceObjectIds.has(id)
        || selectableSacrificeObjectIds.has(id)
        || selectableManaCostCreatureIds.has(id)
        // An attachment tapped for mana that can still be untapped (undo) is
        // itself actionable — keep it expanded so the undo affordance stays
        // clickable. `undoableTapObjectIds` is already gated upstream
        // (GameBoard `undoLegal`) to the states whose engine match arms accept
        // the untap, so no extra state check is needed here.
        || undoableTapObjectIds.has(id),
    );
  const attachmentsLifted =
    obj.attachments.length > 0
    && (attachmentsLiftedByAncestor || isInHoveredAttachmentTree);
  const attachmentsExpanded = obj.attachments.length <= 1 || isSelected || attachmentsActionable;
  const visibleAttachmentIds = attachmentsExpanded ? obj.attachments : obj.attachments.slice(0, 1);
  const attachmentPathIds = new Set([...attachmentRenderPath, objectId]);
  const renderableAttachmentIds = visibleAttachmentIds.filter((id) => !attachmentPathIds.has(id));
  const hiddenAttachmentCount = obj.attachments.length - visibleAttachmentIds.length;
  const exileLinksExpanded = exileLinks.length <= 1 || isHovered || isSelected || isInspected;
  const visibleExileLinks = exileLinksExpanded ? exileLinks : exileLinks.slice(0, 1);
  const hiddenExileCount = exileLinks.length - visibleExileLinks.length;

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
  } else if (isSelectedForBoardChoice && boardChoice) {
    glowClass = selectedBoardChoiceGlowClass(boardChoice.intent);
  } else if (isSelectableForBoardChoice && boardChoice) {
    glowClass = availableBoardChoiceGlowClass(boardChoice.intent);
  } else if (isSelectedForManaCost) {
    glowClass =
      "ring-2 ring-emerald-400 shadow-[0_0_14px_4px_rgba(52,211,153,0.55)]";
  } else if (isSelectableForManaCost) {
    glowClass =
      "ring-2 ring-emerald-300/70 shadow-[0_0_10px_3px_rgba(74,222,128,0.35)]";
  } else if (isValidTarget) {
    glowClass =
      "outline outline-2 outline-black/80 ring-4 ring-lime-300 shadow-[0_0_18px_6px_rgba(190,242,100,0.72),inset_0_0_18px_4px_rgba(190,242,100,0.22)]";
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

  // CR 702.26: Per-permanent phasing — phased-out permanents stay on the
  // battlefield but are treated as though they don't exist (CR 702.26d). We
  // surface this with the same sky-blue "ethereal plane" tint used for
  // player-area phasing (PlayerArea.tsx), plus a mild opacity drop so the
  // card stays readable. Player-area phasing is rendered separately on
  // PlayerArea; both can be active independently.
  const isPhasedOut = obj.phase_status?.status === "PhasedOut";

  // CR 707.2: A token-copy of a real card (Twinflame, Helm of the Host, or a
  // debug `CreateTokenCopy`) is `is_token` yet keeps `display_source = "Card"`,
  // so it renders pixel-identical to the printed permanent. Flag it so the
  // board carries a "Copy" badge — generic tokens (Treasure, Goblin) already
  // read as tokens via their distinct generic-token art and are excluded.
  // CR 708.2: a face-down permanent has no characteristics other than those
  // its face-down rule grants, so never surface "Copy" on it — that would leak
  // that it's a token-copy (matches the `!face_down` guard on the keyword strip).
  const isCopy = obj.is_token === true && obj.display_source !== "Token" && !obj.face_down;

  // Filter out loyalty counters — shown separately as the loyalty badge
  const counters = Object.entries(obj.counters).filter((entry): entry is [string, number] => entry[1] != null && entry[0] !== "loyalty");

  // Tap rotation: 17deg in MTGA mode (or compact-height), 90deg in classic mode
  const tapBaseOpacity = (isCompactHeight || tapRotation === "mtga") && obj.tapped ? 0.85 : 1;
  // CR 702.26: Phased-out permanents render at 70% opacity (matching the
  // player-area phasing treatment in PlayerArea.tsx commit 4d6cfb506) so the
  // sky-blue tint reads as "ethereal" rather than overpowering the art.
  const tapOpacity = isPhasedOut ? Math.min(tapBaseOpacity, 0.7) : tapBaseOpacity;
  const isRotatedFull = obj.tapped;

  // Attacker slide-forward: player creatures slide up, opponent creatures slide down.
  // Reduced on compact-height where 30px would overflow the small creature row.
  const attackSlideMagnitude = isCompactHeight ? 12 : 30;
  const attackSlide = isAttacking ? (obj.controller === playerId ? -attackSlideMagnitude : attackSlideMagnitude) : 0;

  const handleClick = (e: React.MouseEvent) => {
    if (longPressFired.current) { longPressFired.current = false; return; }
    if (useUiStore.getState().debugInteractionMode) {
      e.stopPropagation();
      useUiStore.getState().openDebugContextMenu({ objectId, x: e.clientX, y: e.clientY });
      return;
    }
    if (onPrimaryClickOverride) {
      e.stopPropagation();
      onPrimaryClickOverride();
      return;
    }
    // Attached cards (Auras / Equipment / Fortifications) render as nested
    // <PermanentCard> inside their host's wrapper so they get full
    // click/hover/target handling for free. Without stopping propagation, a
    // click on an attachment would bubble to the host and `selectObject(host)`
    // would steal focus — preventing the player from selecting the Equipment
    // to activate Equip and reattach it. Stop the bubble so the attachment's
    // own intent (target / activate / select) wins cleanly.
    if (obj.attached_to !== null) e.stopPropagation();
    // A permanent and its attachments are each independently clickable in place
    // — the host by its face, an attached Equipment/Aura by its right-edge peek
    // (CR 301.5 / 303.4: an attachment is its own legal object). We deliberately
    // do NOT hijack an ambiguous click into the AttachmentFan here: direct
    // targeting must always work. When the peek is an awkward click target the
    // player can open the fan explicitly via the attachment badge instead of being
    // forced through it.
    // A PayCost TapCreatures prompt is mid-cost resolution — check before combat
    // mode so clicks land even when DeclareAttackers combat mode is active.
    if (isSelectableForBoardChoice && boardChoice) {
      if (isBoardChoiceImmediate(boardChoice)) {
        dispatchAction(buildBoardChoiceAction(boardChoice, [objectId]));
      } else {
        const maxSelection = boardChoiceMaxSelection(boardChoice);
        if (
          isSelectedForBoardChoice
          || maxSelection == null
          || selectedBoardChoiceIds.length < maxSelection
        ) {
          toggleSelectedCard(objectId);
        }
      }
    } else if (isSelectableForManaCost && tapCreatureCostChoice) {
      if (
        isSelectedForManaCost
        || selectedCardIds.length < tapCreatureCostChoice.count
      ) {
        toggleSelectedCard(objectId);
      }
    } else if (combatMode === "attackers" && waitingFor?.type === "DeclareAttackers") {
      if (isValidAttacker) toggleAttacker(objectId);
    } else if (combatMode === "blockers" && waitingFor?.type === "DeclareBlockers" && combatClickHandler) {
      combatClickHandler(objectId);
    } else if (equipTargetChoice?.valid_targets.includes(objectId)) {
      dispatchAction({
        type: "Equip",
        data: {
          equipment_id: equipTargetChoice.equipment_id,
          target_id: objectId,
        },
      });
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
      const manaActions: GameAction[] = [];
      const keywordActions: GameAction[] = [];
      for (const action of objectActions) {
        if (isManaObjectAction(action, o)) {
          manaActions.push(action);
        } else if (action.type === "ActivateAbility") {
          abilityActions.push(action);
        } else {
          // CR 113.3b keyword activations (Crew/Station/Equip/Saddle) and any
          // future per-permanent action are surfaced alongside activated
          // abilities in the choice modal.
          keywordActions.push(action);
        }
      }
      const manaChoiceNeeded = manaActions.length > 1;

      const nonManaActions: GameAction[] = [...abilityActions, ...keywordActions];
      if (nonManaActions.length === 0 && canTapForMana) {
        if (manaChoiceNeeded) {
          setPendingAbilityChoice({ objectId, actions: manaActions });
        } else if (manaActions.length === 1) {
          dispatchAction(manaActions[0]);
        }
      } else {
        // #506: lone-action auto-dispatch is gated through
        // resolveSingleActionDispatch so a card-consuming ActivateAbility
        // surfaces the choice modal instead of auto-firing. This merges the
        // former `nonManaActions.length === 1 && !canTapForMana` branch — when
        // canTapForMana is false, allActions === nonManaActions, so a lone
        // non-mana action reproduces that branch exactly.
        const allActions: GameAction[] = [...nonManaActions];
        if (canTapForMana) {
          allActions.push(...manaActions);
        }
        const auto = resolveSingleActionDispatch(allActions, o);
        if (auto) {
          dispatchAction(auto);
        } else {
          setPendingAbilityChoice({ objectId, actions: allActions });
        }
      }
    } else if (isUndoableTap) {
      dispatchAction({ type: "UntapLandForMana", data: { object_id: objectId } });
    } else if (isMobile) {
      inspectObject(objectId);
      setPreviewSticky(true);
    } else {
      selectObject(isSelected ? null : objectId);
    }
  };

  const useArtCrop = battlefieldCardDisplay === "art_crop";
  const highlightRadiusClass = useArtCrop ? "rounded-[6px]" : "rounded-lg";

  // ⧉ badge scales with the active card width var (same idiom as the keyword
  // strip / tap glyph) so it stays a corner affordance instead of covering
  // half of a small battlefield card.
  const attachmentBadgeSize = useArtCrop
    ? "clamp(20px, calc(var(--art-crop-w) * 0.24), 26px)"
    : "clamp(20px, calc(var(--card-w) * 0.22), 28px)";
  const attachmentBadgeFontSize = useArtCrop
    ? "clamp(12px, calc(var(--art-crop-w) * 0.13), 14px)"
    : "clamp(12px, calc(var(--card-w) * 0.12), 15px)";

  return (
    <motion.div
      ref={cardRef}
      data-object-id={objectId}
      data-grouped-ids={coveredIds && coveredIds.length > 1 ? coveredIds.join(" ") : undefined}
      data-card-hover
      layoutId={`permanent-${objectId}`}
      className="relative inline-flex w-fit cursor-pointer overflow-visible rounded-lg self-end select-none"
      style={{
        zIndex: attachmentsLifted ? HOVERED_ATTACHMENT_HOST_Z_INDEX : isHovered ? HOVERED_CARD_Z_INDEX : isAttacking ? 50 : undefined,
        transformOrigin: "center center",
        // Reserve space below for exile ghost cards
        marginBottom:
          visibleExileLinks.length > 0
            ? `${visibleExileLinks.length * EXILE_GHOST_OFFSET_PX}px`
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
      {isManaPaymentPreviewSource && (
        <div
          aria-hidden
          className="pointer-events-none absolute inset-0 z-[70] rounded-lg outline outline-2 outline-orange-400 shadow-[0_0_12px_3px_rgba(251,146,60,0.55)]"
        />
      )}
      {/* Attachments stagger out to the right of the host with their right
          edge peeking past the host's right edge. The recursive PermanentCard
          render gives each attachment full click/hover/target handling for
          free, mirroring how an Aura/Equipment behaves anywhere else on the
          battlefield.

          Card 0 (innermost) is closest to the host with the smallest peek;
          subsequent cards shift further right so each one's right edge is
          visible past the previous one. z-index counts DOWN from a value
          below the host's z-10 so attachments stay tucked behind the host
          face. While the host or one of its attachment descendants is
          hovered, lift only the outer permanent tree above sibling
          permanents; internal host/attachment ordering stays unchanged. */}
      {renderableAttachmentIds.map((attachId, i) => {
        const peekPx = ATTACHMENT_PEEK_PX + i * ATTACHMENT_STACK_STEP_PX;
        return (
          <div
            key={attachId}
            className="absolute top-0"
            style={{
              left: "100%",
              transform: `translateX(calc(-100% + ${peekPx}px))`,
              zIndex: 5 - i,
            }}
          >
            <PermanentCard
              objectId={attachId}
              attachmentsLiftedByAncestor={attachmentsLifted}
              attachmentRenderPath={[...attachmentRenderPath, objectId]}
            />
            <AttachmentTypeBadge attachId={attachId} />
          </div>
        );
      })}
      {hiddenAttachmentCount > 0 && (
        <button
          type="button"
          className="absolute -right-3 top-6 z-30 flex h-6 min-w-6 items-center justify-center rounded-full bg-amber-300 px-1.5 text-[11px] font-black leading-none text-amber-950 ring-2 ring-amber-950/80 shadow transition-transform hover:scale-105 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white"
          title={t("permanent.hiddenAttachmentsAria", { count: hiddenAttachmentCount })}
          aria-label={t("permanent.hiddenAttachmentsAria", { count: hiddenAttachmentCount })}
          onPointerDown={(event) => event.stopPropagation()}
          onClick={openAttachmentFan}
        >
          +{hiddenAttachmentCount}
        </button>
      )}

      {/* Exile ghosts — cards held in exile by this permanent, peeking from below */}
      {visibleExileLinks.map((link, i) => (
        <ExileGhostCard
          key={link.exiled_id}
          objectId={link.exiled_id}
          offset={(i + 1) * EXILE_GHOST_OFFSET_PX}
        />
      ))}
      {hiddenExileCount > 0 && (
        <div
          className="pointer-events-none absolute left-8 z-30 flex h-6 min-w-6 items-center justify-center rounded-full bg-purple-300 px-1.5 text-[11px] font-black leading-none text-purple-950 ring-2 ring-purple-950/80 shadow"
          style={{ bottom: `-${(visibleExileLinks.length + 1) * EXILE_GHOST_OFFSET_PX}px` }}
          title={t("permanent.hiddenExileCards", { count: hiddenExileCount })}
          aria-label={t("permanent.hiddenExileCards", { count: hiddenExileCount })}
        >
          +{hiddenExileCount}
        </div>
      )}

      {/* Main card — art crop or full card based on preference */}
      {useArtCrop ? (
        <div className="relative z-10 rounded-lg">
          <ArtCropCard objectId={objectId} />
          {/* CR 702.26: phased-out tint overlay — sky-blue mix-blend-screen
              matches the player-area treatment (PlayerArea.tsx 4d6cfb506). */}
          {isPhasedOut && (
            <div
              data-phased-out="true"
              className="absolute inset-0 z-20 bg-sky-500/25 mix-blend-screen pointer-events-none rounded-lg"
            />
          )}
          {isRingBearer && (
            <div
              className="absolute bottom-1 left-1 z-20 rounded bg-amber-500/90 px-1.5 py-0.5 text-[10px] font-black uppercase tracking-wide text-amber-950 shadow ring-1 ring-amber-100/70"
              title={t("permanent.ringBearerTooltip")}
            >
              {t("permanent.ringBearer")}
            </div>
          )}
        </div>
      ) : (
        <>
          <div className="relative z-10 rounded-lg overflow-hidden">
            <CardImage cardName={imgName} faceIndex={imgFace} oracleId={imgOracleId} faceName={imgFaceName} size="small" unimplementedMechanics={obj.unimplemented_mechanics} colors={displayColors} isToken={obj.display_source === "Token"} tokenFilters={obj.display_source === "Token" ? tokenFiltersForObject(obj) : undefined} tokenImageRef={obj.token_image_ref} oracleText={obj.display_source === "Token" ? obj.token_rules_text : undefined} faceDown={obj.face_down} />
            {/* CR 702.26: phased-out tint overlay — sky-blue mix-blend-screen
                matches the player-area treatment (PlayerArea.tsx 4d6cfb506). */}
            {isPhasedOut && (
              <div
                data-phased-out="true"
                className="absolute inset-0 z-20 bg-sky-500/25 mix-blend-screen pointer-events-none rounded-lg"
              />
            )}
          </div>

          {/* P/T box for creatures */}
          {ptDisplay && (
            <PTBox
              ptDisplay={ptDisplay}
              position={obj.loyalty != null ? "left" : "right"}
              ptSources={ptSources}
              basePower={obj.base_power}
              baseToughness={obj.base_toughness}
            />
          )}

          {/* Damage overlay for non-creatures only (creatures use P/T box) */}
          {!ptDisplay && obj.damage_marked > 0 && (
            <div className="absolute inset-x-0 bottom-0 z-20 flex h-6 items-center justify-center rounded-b-lg bg-red-600/60 text-xs font-bold text-white">
              -{obj.damage_marked}
            </div>
          )}

          {obj.loyalty != null && (
            <LoyaltyBadge
              amount={obj.loyalty}
              kind="total"
              size="battlefield"
              className="absolute bottom-0 right-0 z-30"
              style={{ position: "absolute" }}
            />
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
              title={t("permanent.underAttack", { count: incomingAttackerCount })}
            >
              <span aria-hidden>⚔</span>
              {incomingAttackerCount > 1 && <span>×{incomingAttackerCount}</span>}
            </div>
          )}

          {isRingBearer && (
            <div
              className="absolute bottom-1 left-1 z-20 rounded bg-amber-500/90 px-1.5 py-0.5 text-[10px] font-black uppercase tracking-wide text-amber-950 shadow ring-1 ring-amber-100/70"
              title={t("permanent.ringBearerTooltip")}
            >
              {t("permanent.ringBearer")}
            </div>
          )}

          {/* Top-right overlay stack: counter badges kept clear of the
              bottom-right P/T box. */}
          <div className="absolute right-0.5 top-0.5 z-[60] flex flex-col items-end gap-0.5">
            {counters.map(([type, count]) => {
              const iconClass = counterIconClass(type);
              return (
                <CounterTooltip key={type} type={type} count={count}>
                  <span
                    className={`flex items-center gap-0.5 rounded px-1 text-[10px] font-bold text-white ${COUNTER_COLORS[type] ?? "bg-purple-600"}`}
                  >
                    {iconClass && (
                      <ManaFontIcon
                        iconClass={iconClass}
                        fallbackText=""
                        label={formatCounterType(type)}
                      />
                    )}
                    {formatCounterType(type)} x{count}
                  </span>
                </CounterTooltip>
              );
            })}
          </div>

        </>
      )}

      {/* Keyword badges: a vertical column of square glyph badges straddling
          the card's top-left edge. Rendered at the SHARED motion.div level
          (after the art-crop/full-card ternary) so it appears in BOTH display
          modes, and — being at the overflow-visible level, outside the rounded
          overflow-hidden art wrapper — the half-off-card portion isn't clipped.
          Badge size scales off the active card width var. */}
      {showKeywordStrip && battlefieldKeywordBadges.length > 0 && !obj.face_down && (
        <KeywordStrip
          keywords={battlefieldKeywordBadges}
          baseKeywords={obj.base_keywords}
          sourceByKeyword={keywordSourceMap}
          badgeSize={
            useArtCrop
              ? "clamp(11px, calc(var(--art-crop-w) * 0.22), 22px)"
              : "clamp(13px, calc(var(--card-w) * 0.2), 26px)"
          }
          maxVisible={useArtCrop ? 4 : 5}
        />
      )}

      {hasSummoningSickness && (
        <SummoningSicknessOverlay variant={useArtCrop ? "artCrop" : "fullCard"} />
      )}

      {/* Tapped indicator: a light wash + a centered tap glyph. The glyph
          counter-rotates by the card's tap angle so it reads upright even when
          the whole card is turned 90°. */}
      {obj.tapped && !obj.face_down && (
        <div
          aria-hidden
          className="pointer-events-none absolute inset-0 z-20 flex items-center justify-center rounded-lg bg-white/20"
        >
          <ManaFontIcon
            iconClass="ms-tap"
            fallbackText=""
            className="text-white/90 drop-shadow-[0_1px_4px_rgba(0,0,0,0.95)]"
            style={{
              fontSize: useArtCrop
                ? "clamp(16px, calc(var(--art-crop-w) * 0.42), 40px)"
                : "clamp(20px, calc(var(--card-w) * 0.4), 56px)",
              transform: `rotate(${-tapAngle}deg)`,
            }}
          />
        </div>
      )}

      {glowClass && (
        <div
          aria-hidden
          data-card-affordance-highlight="true"
          className={`pointer-events-none absolute inset-0 z-30 ${highlightRadiusClass} ${glowClass}`}
        />
      )}

      {isValidTarget && (
        <div
          className={`pointer-events-none absolute ${isUnderAttack ? "left-1 top-7" : "left-1 top-1"} z-40 rounded bg-lime-300 px-1.5 py-0.5 text-[9px] font-black uppercase leading-none tracking-normal text-black ring-1 ring-black/70 shadow-[0_1px_4px_rgba(0,0,0,0.75)]`}
        >
          {t("permanent.target")}
        </div>
      )}

      {isSelectableForBoardChoice && boardChoice && (
        // Selected cards get a checkmark + solid, white-ringed badge; eligible-
        // but-unselected cards get the same opaque label, so the current
        // selection is unambiguous and the badge reads as a toggle.
        <div
          className={`pointer-events-none absolute ${isUnderAttack || isValidTarget ? "right-1 top-7" : "right-1 top-1"} z-40 rounded ${boardChoiceBadgeClass(boardChoice.intent)} px-1.5 py-0.5 text-[9px] font-black uppercase leading-none tracking-normal shadow-[0_1px_4px_rgba(0,0,0,0.75)] ${isSelectedForBoardChoice ? "ring-1 ring-white/90" : "ring-1 ring-black/70"}`}
        >
          {isSelectedForBoardChoice ? `✓ ${t(`permanent.boardChoiceBadges.${boardChoice.intent}`)}` : t(`permanent.boardChoiceBadges.${boardChoice.intent}`)}
        </div>
      )}

      {/* CR 707.2: "Copy" badge for token-copies of real cards — these are
          pixel-identical to the printed permanent, so without this tag there's
          no way to tell a copy apart from the original on the board. Hidden
          while the card is a valid target (the lime "Target" tag owns the
          corner during targeting) and shifted down under attack to clear the
          ⚔ badge — same coordination the Target tag uses. */}
      {isCopy && !isValidTarget && (
        <div
          className={`pointer-events-none absolute left-1 ${isUnderAttack ? "top-7" : "top-1"} z-20 rounded bg-indigo-600/90 px-1 py-0.5 text-[9px] font-black uppercase leading-none tracking-wide text-white ring-1 ring-black/60 shadow-[0_1px_4px_rgba(0,0,0,0.6)]`}
          title={t("permanent.copyTooltip")}
        >
          {t("permanent.copy")}
        </div>
      )}

      {/* Debug-panel preview highlight — fuchsia neon ring + animated pulse.
          Triggered when an ObjectSelect option in the debug panel is hovered
          (`debugHighlightedObjectId` state). Deliberately loud and visually
          unrelated to seat/turn/attack/target treatments so it never reads
          as part of the normal game UI. `pointer-events-none` keeps it from
          intercepting clicks/hovers on the card beneath. */}
      {isDebugHighlighted && (
        <div
          aria-hidden
          className="pointer-events-none absolute inset-[-4px] z-40 rounded-xl ring-4 ring-fuchsia-400 shadow-[0_0_22px_6px_rgba(232,121,249,0.7),inset_0_0_18px_4px_rgba(232,121,249,0.45)] animate-pulse"
        />
      )}

      {obj.attachments.length === 1 && (
        <button
          type="button"
          className="absolute -left-2.5 -top-2.5 z-40 flex items-center justify-center rounded-full bg-black/90 leading-none text-amber-200 ring-2 ring-amber-200/80 shadow-[0_2px_8px_rgba(0,0,0,0.65)] transition-transform hover:scale-105 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white"
          style={{
            width: attachmentBadgeSize,
            height: attachmentBadgeSize,
            fontSize: attachmentBadgeFontSize,
          }}
          title={t("permanent.viewAttachmentsFor", { count: 1, name: obj.name })}
          aria-label={t("permanent.viewAttachmentsFor", { count: 1, name: obj.name })}
          onPointerDown={(event) => event.stopPropagation()}
          onClick={openAttachmentFan}
        >
          <span aria-hidden>⧉</span>
        </button>
      )}

    </motion.div>
  );
});

const SummoningSicknessOverlay = memo(function SummoningSicknessOverlay({ variant }: { variant: "artCrop" | "fullCard" }) {
  return (
    <div
      aria-hidden
      data-summoning-sickness-underwater="true"
      className={`summoning-sickness-underwater summoning-sickness-underwater--${variant}`}
    />
  );
});

/**
 * Subtype glyph badge rendered as a circular pill in the top-right of an
 * attached card's peek. Sits where the mana pips would normally be so the
 * player gets a clear "this is an Aura / Equipment / Fortification" hint
 * without parsing the title bar.
 *
 * The badge is sized + colored to read unmistakably as a UI label rather
 * than a sliver of card frame: bright amber on near-black with a sharp
 * ring + drop shadow, and slightly larger than typical inline badges so
 * the glyph is recognizable at a glance.
 *
 * Hidden when the card has no recognized attachment subtype (defensive —
 * current MTG only attaches via Aura / Equipment / Fortification).
 */
const AttachmentTypeBadge = memo(function AttachmentTypeBadge({ attachId }: { attachId: number }) {
  const subtypes = useGameStore((s) => s.gameState?.objects[attachId]?.card_types.subtypes);
  if (!subtypes) return null;
  const glyph = attachmentTypeGlyph(subtypes);
  if (!glyph) return null;
  return (
    <span
      aria-hidden
      // pointer-events-none so the badge doesn't intercept clicks/hovers on
      // the underlying PermanentCard — events must continue to reach the
      // card's own handlers for targeting/selection/preview.
      className="pointer-events-none absolute right-0.5 top-0.5 z-30 flex h-5 w-5 items-center justify-center rounded-full bg-gradient-to-b from-amber-400 to-amber-600 text-[12px] font-bold leading-none text-amber-950 ring-2 ring-amber-200/80 shadow-[0_2px_4px_rgba(0,0,0,0.6),inset_0_1px_1px_rgba(255,255,255,0.5)]"
    >
      {glyph}
    </span>
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
  const controllerIdentity = useGameStore(
    (s) => obj && s.gameState?.players?.find((p) => p.id === obj.controller)?.commander_color_identity,
  );

  if (!obj) return null;

  const isLand = obj.card_types.core_types.includes("Land");
  const displayColors = getCardDisplayColors(
    obj.color,
    isLand,
    obj.card_types.subtypes,
    obj.available_mana_pips,
    controllerIdentity || undefined,
  );
  const { name: imgName, faceIndex: imgFace, oracleId: imgOracleId, faceName: imgFaceName } = cardImageLookup(obj);
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
        <CardImage cardName={imgName} faceIndex={imgFace} oracleId={imgOracleId} faceName={imgFaceName} size="small" colors={displayColors} isToken={obj.display_source === "Token"} tokenFilters={obj.display_source === "Token" ? tokenFiltersForObject(obj) : undefined} tokenImageRef={obj.token_image_ref} oracleText={obj.display_source === "Token" ? obj.token_rules_text : undefined} faceDown={obj.face_down} />
      )}
    </div>
  );
});
