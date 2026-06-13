import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { ChosenAttribute, GameObject, Keyword, ManaCost, Zone } from "../../adapter/types.ts";
import { collectObjectActions } from "../../viewmodel/cardActionChoice.ts";
import { abilityLabel } from "../../viewmodel/costLabel.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import type { SourcePrinting } from "../../hooks/useCardImage.ts";
import { useIsMobile } from "../../hooks/useIsMobile.ts";
import { useEngineCardData, useCardParseDetails, useCardRulings, type ParsedItem } from "../../hooks/useEngineCardData.ts";
import { tokenFiltersForObject } from "../../services/cardImageLookup.ts";
import type { CardRuling } from "../../services/engineRuntime.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { ManaCostPips } from "../mana/ManaCostPips.tsx";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";
import { computePTDisplay, formatCounterType, formatTypeLine, toRoman } from "../../viewmodel/cardProps.ts";
import {
  getKeywordDisplayText,
  getKeywordName,
  getKeywordReminderText,
  isGrantedKeyword,
  sortKeywords,
} from "../../viewmodel/keywordProps.ts";
import {
  buildGrantedKeywordSources,
  buildPTSources,
  formatPTDelta,
} from "../../viewmodel/attribution.ts";

let lastPointerPosition: { x: number; y: number } | null = null;

if (typeof window !== "undefined") {
  window.addEventListener(
    "mousemove",
    (event) => {
      lastPointerPosition = { x: event.clientX, y: event.clientY };
    },
    { passive: true },
  );
}

export interface CardHoverInfo {
  name: string;
  sourcePrinting?: SourcePrinting;
}

interface CardPreviewProps {
  cardName: string | null;
  backFaceName?: string | null;
  faceIndex?: number;
  position?: { x: number; y: number };
  scryfallId?: string;
  sourcePrinting?: SourcePrinting;
  /** When true, the desktop preview docks to the screen edge (the default
   *  top-right rail position) instead of following the cursor — keeps it from
   *  covering the board. Drives the "side" card-preview preference. Ignored
   *  when an explicit `position` is given or on mobile. */
  dockSide?: boolean;
  /** Overrides the mobile-overlay dismiss handler. Contexts that drive the
   *  preview via their own state (e.g. the deck builder's hoveredCard) pass
   *  this so a tap-to-dismiss clears THAT state; defaults to the in-game
   *  uiStore.dismissPreview. */
  onDismiss?: () => void;
  /** Mobile/touch presentation. "modal" (default) is the full-screen,
   *  tap-to-dismiss overlay used in-game. "compact" is a smaller, non-blocking
   *  floating card that auto-dismisses on the next tap or scroll — used by the
   *  deck builder, where you browse many cards quickly and a full-screen
   *  takeover requiring a separate dismiss tap is too heavy. */
  mobileLayout?: "modal" | "compact";
}

export function CardPreview({
  cardName,
  backFaceName,
  faceIndex,
  position,
  scryfallId,
  sourcePrinting,
  dockSide,
  onDismiss,
  mobileLayout = "modal",
}: CardPreviewProps) {
  if (!cardName) return null;

  return (
    <CardPreviewInner
      cardName={cardName}
      backFaceName={backFaceName ?? null}
      faceIndex={faceIndex}
      position={position}
      scryfallId={scryfallId}
      sourcePrinting={sourcePrinting}
      dockSide={dockSide}
      onDismiss={onDismiss}
      mobileLayout={mobileLayout}
    />
  );
}

function CardPreviewInner({
  cardName,
  backFaceName: backFaceNameProp,
  faceIndex,
  position,
  scryfallId,
  sourcePrinting,
  dockSide,
  onDismiss,
  mobileLayout,
}: {
  cardName: string;
  backFaceName: string | null;
  faceIndex?: number;
  position?: { x: number; y: number };
  scryfallId?: string;
  sourcePrinting?: SourcePrinting;
  dockSide?: boolean;
  onDismiss?: () => void;
  mobileLayout?: "modal" | "compact";
}) {
  const { t } = useTranslation("game");
  const inspectedObjectId = useUiStore((s) => s.inspectedObjectId);
  const dismissPreview = useUiStore((s) => s.dismissPreview);
  const showDebugId = useUiStore((s) => s.debugPanelOpen || s.debugInteractionMode);
  const obj = useGameStore((s) =>
    inspectedObjectId != null ? s.gameState?.objects[inspectedObjectId] ?? null : null,
  );

  // Auto-derive back face name from " // " separator when not explicitly provided
  // (e.g., deck builder passes "Delver of Secrets // Insectile Aberration" as cardName)
  const backFaceName = backFaceNameProp ?? (
    cardName.includes(" // ") ? cardName.split(" // ")[1] : null
  );

  // For DFC names ("Front // Back"), extract the front face name for engine lookup
  const frontFaceName = cardName.includes(" // ") ? cardName.split(" // ")[0] : cardName;

  // When no game object exists (deck builder context), look up engine-parsed data via WASM.
  // Fetch both faces so Alt+Ctrl shows the back face's parsed data.
  const engineFrontFace = useEngineCardData(obj ? null : frontFaceName);
  const engineBackFace = useEngineCardData(obj ? null : backFaceName);

  // Parse details: hierarchical tree with per-item support status.
  // For in-game objects, look up by obj.name; for deck builder, use the face names.
  const lookupName = obj?.name ?? frontFaceName;
  const frontParseDetails = useCardParseDetails(lookupName);
  const backParseDetails = useCardParseDetails(backFaceName);

  const isToken = obj?.display_source === "Token";
  // For transformed DFCs, the active face is the back (Scryfall faceIndex 1).
  // The engine swaps obj.name to the active face, but Scryfall always indexes
  // 0=front, 1=back regardless of search name — so we must flip the index.
  const isTransformed = obj?.transformed ?? false;
  const defaultFaceIndex = faceIndex ?? (isTransformed ? 1 : 0);
  // Battlefield path: route through oracle_id when the engine attached one.
  // Deck-builder path: `obj` is null, so we keep the name-based fallback.
  const { src, isLoading, isRotated, isFlip } = useCardImage(cardName, {
    size: "normal",
    faceIndex: defaultFaceIndex,
    isToken,
    tokenFilters: isToken && obj ? tokenFiltersForObject(obj) : undefined,
    tokenImageRef: isToken && obj ? obj.token_image_ref : undefined,
    oracleId: obj?.printed_ref?.oracle_id,
    faceName: obj?.printed_ref?.face_name,
    scryfallId,
    sourcePrinting,
  });
  const classLevel = obj?.class_level;
  const previewRef = useRef<HTMLDivElement | null>(null);
  const pointerRef = useRef<{ x: number; y: number } | null>(null);
  const frameRef = useRef<number | null>(null);
  const altHeld = useUiStore((s) => s.altHeld);
  const [ctrlHeld, setCtrlHeld] = useState(false);
  const isMobile = useIsMobile();

  useEffect(() => {
    if (typeof window === "undefined") return undefined;

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Control") setCtrlHeld(true);
    }

    function handleKeyUp(event: KeyboardEvent) {
      if (event.key === "Control") setCtrlHeld(false);
    }

    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("keyup", handleKeyUp);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("keyup", handleKeyUp);
    };
  }, []);

  // Kamigawa flip cards print both halves in one image, the alternate half
  // rotated 180°. There's no second face to fetch, so Ctrl spins the same image
  // 180° (flip180) instead of swapping faces the way DFC/MDFC do (showOtherFace).
  const flip180 = !isMobile && ctrlHeld && isFlip;
  // On desktop, Ctrl swaps to the other face (back face normally, front face if transformed)
  const showOtherFace = !isMobile && ctrlHeld && backFaceName != null && !isFlip;
  // Fetch other face image when Ctrl is held (hook must always be called, but with empty
  // string when not needed so useCardImage short-circuits without a network request).
  // Battlefield path: the back_face's printed_ref carries the other face's
  // oracle_id (same as front for DFC/MDFC) and the other face's name. Deck-
  // builder path falls back to name + flipped faceIndex.
  const otherFaceIndex = isTransformed ? 0 : 1;
  const otherFaceOracleId = obj?.back_face?.printed_ref?.oracle_id;
  const otherFaceName = obj?.back_face?.printed_ref?.face_name;
  const otherFaceImgResult = useCardImage(showOtherFace ? backFaceName! : "", {
    size: "normal",
    faceIndex: otherFaceIndex,
    oracleId: showOtherFace ? otherFaceOracleId : undefined,
    faceName: showOtherFace ? otherFaceName : undefined,
  });

  const activeSrc = showOtherFace ? otherFaceImgResult.src : src;
  const activeLoading = showOtherFace ? otherFaceImgResult.isLoading : isLoading;
  const activeRotated = showOtherFace ? otherFaceImgResult.isRotated : isRotated;
  const displayName = showOtherFace ? backFaceName! : cardName;
  const showInfoPanel = obj?.zone === "Battlefield";
  const infoPanelHeight = showInfoPanel ? 120 : 0;
  const portraitPreviewWidth =
    typeof window === "undefined" ? 472 : Math.min(Math.max(window.innerWidth * 0.26, 220), 472);
  const previewWidth = activeRotated ? portraitPreviewWidth * 1.4 : portraitPreviewWidth;
  const previewHeight =
    (activeRotated
      ? portraitPreviewWidth
      : typeof window === "undefined"
        ? 661
        : Math.min(window.innerHeight * 0.8, portraitPreviewWidth * (7 / 5)))
    + infoPanelHeight;
  const viewportWidth = typeof window === "undefined" ? 1440 : window.innerWidth;
  const viewportHeight = typeof window === "undefined" ? 900 : window.innerHeight;
  const gap = 20;
  const margin = 16;
  const defaultDesktopStyle: React.CSSProperties = {
    right: "calc(env(safe-area-inset-right) + 1rem + var(--game-right-rail-offset, 0px))",
    top: "calc(env(safe-area-inset-top) + var(--game-top-overlay-offset, 0px) + 1rem)",
  };

  useEffect(() => {
    // `dockSide` keeps the preview pinned to `defaultDesktopStyle` (the
    // top-right rail) by skipping the cursor-follow positioning entirely.
    if (typeof window === "undefined" || position || isMobile || dockSide) return undefined;

    pointerRef.current = lastPointerPosition;

    const applyPreviewPosition = () => {
      frameRef.current = null;
      const preview = previewRef.current;
      const pointer = pointerRef.current;
      if (!preview || !pointer) return;

      // Clamp against the ACTUAL rendered size, not the image-only estimate:
      // the "Alt: parsed abilities" / "Hold Ctrl" hint bars add height below the
      // card, and clamping on the estimate let that overflow the bottom of short
      // (e.g. tablet) viewports.
      const measuredWidth = preview.offsetWidth || previewWidth;
      const measuredHeight = preview.offsetHeight || previewHeight;
      const left =
        pointer.x > viewportWidth / 2
          ? Math.max(16, pointer.x - measuredWidth - gap)
          : Math.min(pointer.x + gap, viewportWidth - measuredWidth - 16);
      const top = altHeld
        ? margin
        : Math.min(
            Math.max(margin, pointer.y - measuredHeight / 2),
            viewportHeight - measuredHeight - margin,
          );

      preview.style.right = "auto";
      preview.style.left = `${left}px`;
      preview.style.top = `${top}px`;
    };

    const schedulePositionUpdate = () => {
      if (frameRef.current != null) return;
      frameRef.current = window.requestAnimationFrame(applyPreviewPosition);
    };

    const handlePointerMove = (event: MouseEvent) => {
      pointerRef.current = { x: event.clientX, y: event.clientY };
      schedulePositionUpdate();
    };

    window.addEventListener("mousemove", handlePointerMove);
    schedulePositionUpdate();

    // The preview grows when async content settles (image load, hint bars, face
    // swap); re-clamp on size change so a late-appearing hint bar can't leave the
    // card hanging off the bottom.
    const resizeObserver =
      previewRef.current != null
        ? new ResizeObserver(() => schedulePositionUpdate())
        : null;
    if (resizeObserver && previewRef.current) resizeObserver.observe(previewRef.current);

    return () => {
      window.removeEventListener("mousemove", handlePointerMove);
      resizeObserver?.disconnect();
      if (frameRef.current != null) {
        window.cancelAnimationFrame(frameRef.current);
        frameRef.current = null;
      }
    };
  }, [
    altHeld,
    dockSide,
    gap,
    isMobile,
    margin,
    position,
    previewHeight,
    previewWidth,
    viewportHeight,
    viewportWidth,
  ]);

  // Mobile overlay mode: centered with backdrop
  if (isMobile) {
    return (
      <MobilePreviewOverlay
        cardName={cardName}
        backFaceName={backFaceName}
        faceIndex={defaultFaceIndex}
        obj={obj}
        onDismiss={onDismiss ?? dismissPreview}
        sourcePrinting={sourcePrinting}
        layout={mobileLayout ?? "modal"}
      />
    );
  }

  const style: React.CSSProperties = position
    ? (() => {
        const estimatedWidth = Math.min(previewWidth, viewportWidth - margin * 2);
        const estimatedHeight = Math.min(previewHeight, viewportHeight - margin * 2);
        const unclampedLeft =
          position.x > viewportWidth / 2
            ? position.x - previewWidth - gap
            : position.x + gap;
        const unclampedTop = altHeld ? margin : position.y - estimatedHeight / 2;

        return {
          left: Math.min(
            Math.max(margin, unclampedLeft),
            Math.max(margin, viewportWidth - estimatedWidth - margin),
          ),
          top: Math.min(
            Math.max(margin, unclampedTop),
            Math.max(margin, viewportHeight - estimatedHeight - margin),
          ),
        };
      })()
    : defaultDesktopStyle;

  return (
    <div
      ref={previewRef}
      className="fixed z-[100] pointer-events-none"
      style={style}
      data-card-preview
    >
      {altHeld && (frontParseDetails || engineFrontFace) ? (
        <ParsedAbilitiesPanel
          name={showOtherFace ? (engineBackFace?.name ?? backFaceName ?? "") : (obj?.name ?? engineFrontFace?.name ?? frontFaceName)}
          cardTypes={showOtherFace ? engineBackFace?.card_type : (obj?.card_types ?? engineFrontFace?.card_type)}
          keywords={showOtherFace ? undefined : obj?.keywords}
          localizedTypeLine={showOtherFace ? engineBackFace?.localized_type_line : engineFrontFace?.localized_type_line}
          parseDetails={showOtherFace && backParseDetails ? backParseDetails : frontParseDetails}
          maxHeight={viewportHeight - margin * 2}
        />
      ) : (
        <CardImagePreview
          cardName={displayName}
          classLevel={classLevel}
          showInfoPanel={showInfoPanel}
          obj={obj}
          showOtherFace={showOtherFace}
          otherFaceCost={obj?.back_face?.mana_cost ?? null}
          isLoading={activeLoading}
          src={activeSrc}
          isRotated={activeRotated}
          flip180={flip180}
          backFaceHint={isFlip
            ? (flip180 ? null : t("preview.holdCtrlFlip"))
            : backFaceName != null && !showOtherFace
              ? (isTransformed ? t("preview.holdCtrlFront") : t("preview.holdCtrlBack"))
              : null}
          altAvailable={Boolean(frontParseDetails || engineFrontFace)}
          debugObjectId={showDebugId && inspectedObjectId != null ? inspectedObjectId : null}
        />
      )}
    </div>
  );
}

/** Mobile/tablet: card anchored right (landscape) or center (portrait), whole card visible. */
function MobilePreviewOverlay({
  cardName,
  faceIndex,
  obj,
  onDismiss,
  sourcePrinting,
  layout = "modal",
}: {
  cardName: string;
  backFaceName: string | null;
  faceIndex?: number;
  obj: GameObject | null;
  onDismiss: () => void;
  sourcePrinting?: SourcePrinting;
  layout?: "modal" | "compact";
}) {
  const { t } = useTranslation("game");
  const { src, isRotated, isFlip } = useCardImage(cardName, {
    size: "normal",
    faceIndex,
    isToken: obj?.display_source === "Token",
    tokenFilters: obj?.display_source === "Token" ? tokenFiltersForObject(obj) : undefined,
    tokenImageRef: obj?.display_source === "Token" ? obj.token_image_ref : undefined,
    oracleId: obj?.printed_ref?.oracle_id,
    faceName: obj?.printed_ref?.face_name,
    sourcePrinting,
  });

  // Mobile has no Ctrl key, so a Kamigawa flip card's 180° spin is a tap toggle
  // (desktop holds Ctrl). Only the full-screen modal layout can host the button —
  // the compact peek dismisses on any tap via document-level capture listeners.
  const [flipped, setFlipped] = useState(false);

  // Compact layout: dismiss on the next tap or scroll anywhere, so no separate
  // dismiss gesture is needed. Listeners attach on a deferred tick so the very
  // tap that opened the preview doesn't immediately close it. Capture phase so
  // scrolls inside the deck's own overflow container are caught too.
  useEffect(() => {
    if (layout !== "compact") return undefined;
    const id = window.setTimeout(() => {
      document.addEventListener("pointerdown", onDismiss, true);
      document.addEventListener("scroll", onDismiss, true);
      document.addEventListener("touchmove", onDismiss, true);
      document.addEventListener("wheel", onDismiss, true);
    }, 0);
    return () => {
      window.clearTimeout(id);
      document.removeEventListener("pointerdown", onDismiss, true);
      document.removeEventListener("scroll", onDismiss, true);
      document.removeEventListener("touchmove", onDismiss, true);
      document.removeEventListener("wheel", onDismiss, true);
    };
  }, [layout, onDismiss]);

  if (layout === "compact") {
    // Non-blocking peek: a smaller card, no dimming backdrop, click-through
    // container (taps fall through to the deck so the next card can be tapped
    // directly). The card itself dismisses on tap.
    return (
      <div
        className="pointer-events-none fixed inset-0 z-[100] flex items-center justify-center p-4"
        data-card-preview
      >
        {src && (
          <img
            src={src}
            alt={cardName}
            draggable={false}
            onPointerDown={onDismiss}
            className={
              isRotated
                ? "pointer-events-auto max-h-[58vw] max-w-[80vh] rotate-90 rounded-xl border border-white/15 object-contain shadow-2xl"
                : "pointer-events-auto max-h-[60vh] max-w-[68vw] rounded-xl border border-white/15 object-contain shadow-2xl"
            }
          />
        )}
      </div>
    );
  }

  // pointerdown (not click): the touch-release that opened this overlay fires
  // pointerup, not pointerdown, so a fresh tap is required to dismiss.
  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center bg-black/40 p-4 landscape:justify-end landscape:p-6"
      data-card-preview
      onPointerDown={onDismiss}
    >
      {src && (
        <div
          className={isRotated
            ? "relative h-[min(60vw,300px)] w-[min(84vw,420px)] max-h-[calc(100dvh-2rem)] max-w-full overflow-hidden rounded-lg shadow-2xl landscape:max-w-[45vw]"
            : "relative max-h-[calc(100dvh-2rem)] max-w-full overflow-hidden rounded-lg shadow-2xl landscape:max-w-[45vw]"}
          onPointerDown={(e) => e.stopPropagation()}
        >
          <img
            src={src}
            alt={cardName}
            draggable={false}
            className={isRotated
              ? "absolute left-1/2 top-1/2 h-[min(84vw,420px)] w-[min(60vw,300px)] -translate-x-1/2 -translate-y-1/2 rotate-90 object-cover"
              : `max-h-[calc(100dvh-2rem)] max-w-full object-contain${isFlip ? " transition-transform duration-200" : ""}${flipped ? " rotate-180" : ""}`}
          />
          {isFlip && (
            <button
              type="button"
              onClick={() => setFlipped((f) => !f)}
              className="pointer-events-auto absolute bottom-3 left-1/2 -translate-x-1/2 rounded-full border border-white/20 bg-black/70 px-4 py-2 text-sm font-semibold text-white shadow-lg backdrop-blur active:bg-black/80"
            >
              ⟳ {t("preview.flip")}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

/** Shared card image preview used by both desktop and mobile modes */
function CardImagePreview({
  cardName,
  classLevel,
  showInfoPanel,
  obj,
  showOtherFace,
  otherFaceCost,
  isLoading,
  src,
  isRotated,
  flip180,
  backFaceHint,
  altAvailable,
  mobileMode,
  debugObjectId,
}: {
  cardName: string;
  classLevel?: number | null;
  showInfoPanel?: boolean;
  obj: GameObject | null;
  showOtherFace?: boolean;
  otherFaceCost?: ManaCost | null;
  isLoading: boolean;
  src: string | null;
  isRotated: boolean;
  flip180?: boolean;
  backFaceHint: string | null;
  altAvailable: boolean;
  mobileMode?: boolean;
  debugObjectId?: number | null;
}) {
  const { t } = useTranslation("game");
  const frameClass = mobileMode
    ? isRotated
      ? "h-[min(40vw,300px)] w-[min(56vw,420px)] max-h-[75vh] max-w-[84vw]"
      : "max-h-[75vh] w-[40vw] max-w-[300px]"
    : isRotated
      ? "h-[clamp(220px,26vw,472px)] w-[clamp(308px,36.4vw,661px)] max-h-[45vw] max-w-[80vh]"
      : "max-h-[80vh] max-w-[42vw] w-[clamp(220px,26vw,472px)] md:max-w-[45vw]";
  const containerClass = showInfoPanel
    ? mobileMode
      ? isRotated
        ? "w-[min(56vw,420px)] max-w-[84vw]"
        : "w-[40vw] max-w-[300px]"
      : isRotated
        ? "w-[clamp(308px,36.4vw,661px)] max-w-[80vh]"
        : "max-w-[42vw] w-[clamp(220px,26vw,472px)] md:max-w-[45vw]"
    : frameClass;
  const imageClass = isRotated
    ? mobileMode
      ? "absolute left-1/2 top-1/2 h-[min(56vw,420px)] w-[min(40vw,300px)] -translate-x-1/2 -translate-y-1/2 rotate-90 object-cover"
      : "absolute left-1/2 top-1/2 h-[clamp(308px,36.4vw,661px)] w-[clamp(220px,26vw,472px)] max-h-[80vh] max-w-[42vw] -translate-x-1/2 -translate-y-1/2 rotate-90 object-cover"
    : `${frameClass} object-cover transition-transform duration-200${flip180 ? " rotate-180" : ""}`;

  // Use effective spell cost from engine if available (reflects alt costs, reductions),
  // otherwise fall back to printed mana cost. When the user holds Ctrl to view the
  // OTHER face of a DFC/MDFC, show THAT face's printed cost — the engine's effective
  // cost only applies to the active face, so for the back face we use its printed
  // mana cost (e.g. The Prismatic Bridge's {W}{U}{B}{R}{G} instead of Esika's
  // {1}{G}{G}). See cardImageLookup / back_face wiring.
  const effectiveCost = useGameStore((s) => obj ? s.spellCosts[String(obj.id)] : undefined);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const activateLabels = useMemo(() => {
    if (!obj || obj.zone !== "Battlefield") return [];
    return collectObjectActions(legalActionsByObject, obj.id)
      .flatMap((action) => {
        if (action.type !== "ActivateAbility") return [];
        const ability = obj.abilities[action.data.ability_index];
        return ability ? [abilityLabel(ability)] : [];
      })
      .filter((label, index, labels) => label && labels.indexOf(label) === index);
  }, [legalActionsByObject, obj]);
  const castManaZones: Zone[] = ["Hand", "Command", "Exile", "Graveyard", "Library"];
  const showCastManaCost =
    !showOtherFace && obj != null && castManaZones.includes(obj.zone);
  const displayCost = showOtherFace
    ? otherFaceCost
    : showCastManaCost
      ? (effectiveCost ?? obj?.mana_cost)
      : null;

  if (isLoading || !src) {
    return (
      <div
        className={`${frameClass} ${isRotated ? "" : "aspect-[5/7]"} rounded-[4%] border border-gray-600 bg-gray-700 shadow-2xl animate-pulse`}
      />
    );
  }

  return (
    <div className={`${containerClass} border border-gray-600 overflow-hidden shadow-2xl ${showInfoPanel ? "rounded-t-[4%] rounded-b-lg bg-gray-900" : "rounded-[4%]"}`}>
      <div className={`${frameClass} relative rounded-[4%] overflow-hidden`}>
        <img
          src={src}
          alt={cardName}
          className={imageClass}
          draggable={false}
        />
        {displayCost && (
          <ManaCostPips cost={displayCost} size="lg" className="absolute right-[7.00%] top-[5.25%] z-10" />
        )}
        {classLevel != null && (
          <div className="absolute bottom-3 left-3 z-10">
            <div className="rounded-t-[4px] rounded-b-none bg-gradient-to-b from-amber-950 to-stone-900 px-3 pt-1.5 pb-2 border border-amber-800/60 shadow-lg clip-bookmark">
              <span className="font-serif text-base font-bold text-amber-300 drop-shadow-[0_1px_2px_rgba(0,0,0,0.8)]">
                {toRoman(classLevel)}
              </span>
            </div>
          </div>
        )}
        {debugObjectId != null && (
          <div className="absolute top-2 left-2 z-10 rounded bg-black/80 px-1.5 py-0.5 font-mono text-[11px] font-bold text-amber-300 ring-1 ring-amber-500/50">
            {t("preview.debugId", { id: debugObjectId })}
          </div>
        )}
      </div>
      {showInfoPanel && obj && (
        <CardInfoPanel
          obj={obj}
          altAvailable={altAvailable}
          activateLabels={activateLabels}
        />
      )}
      {backFaceHint && (
        <div className="bg-gray-900/80 text-center py-1 text-[10px] text-gray-400">{backFaceHint}</div>
      )}
      {!showInfoPanel && altAvailable && (
        <div className="bg-gray-900/80 text-center py-1 text-[10px] text-gray-400">{t("preview.altParsedAbilities")}</div>
      )}
    </div>
  );
}

type ItemCategory = ParsedItem["category"];

/** Stable key for a ParsedItem — category + label is unique within a card's parse tree */
function itemKey(item: ParsedItem, index: number): string {
  return `${item.category}-${item.label}-${index}`;
}

const CATEGORY_STYLES: Record<ItemCategory, { border: string; badge: string; icon: string }> = {
  keyword:     { border: "border-l-violet-400/60", badge: "bg-violet-400/15 text-violet-300", icon: "◆" },
  ability:     { border: "border-l-sky-400/60",    badge: "bg-sky-400/15 text-sky-300",       icon: "✦" },
  trigger:     { border: "border-l-amber-400/60",  badge: "bg-amber-400/15 text-amber-300",   icon: "⚡" },
  static:      { border: "border-l-teal-400/60",   badge: "bg-teal-400/15 text-teal-300",     icon: "🛡" },
  replacement: { border: "border-l-orange-400/60", badge: "bg-orange-400/15 text-orange-300", icon: "↺" },
  cost:        { border: "border-l-rose-400/60",   badge: "bg-rose-400/15 text-rose-300",     icon: "$" },
};

const CATEGORY_ABBR: Record<ItemCategory, string> = {
  keyword: "KW", ability: "EFF", trigger: "TRG", static: "STC", replacement: "RPL", cost: "CST",
};

/** Detail pills rendered as key:value badges */
function DetailPills({ details, badgeClass }: { details: [string, string][]; badgeClass: string }) {
  if (details.length === 0) return null;
  return (
    <div className="mt-1 flex flex-wrap gap-1">
      {details.map(([key, value]) => (
        <span key={key} className={`inline-block rounded-[4px] px-1.5 py-px text-[9px] leading-tight ${badgeClass}`}>
          <span className="opacity-60">{key}:</span> {value}
        </span>
      ))}
    </div>
  );
}

/** Renders a single ParsedItem node with support status and recursive children */
function ParsedItemRow({ item, depth = 0 }: { item: ParsedItem; depth?: number }) {
  const { t } = useTranslation("game");
  const catStyle = CATEGORY_STYLES[item.category];
  const statusColor = item.supported ? "text-emerald-400" : "text-rose-400";

  return (
    <div className={depth ? "ml-3 mt-0.5" : undefined}>
      <div className={`border-l-2 ${catStyle.border} pl-2.5 py-1`}>
        <div className="flex items-start gap-1.5">
          <span className={`text-[10px] mt-px shrink-0 ${statusColor}`}>
            {item.supported ? "●" : "○"}
          </span>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-1.5">
              <span className={`text-[8px] font-bold uppercase tracking-wider ${statusColor} opacity-70`}>
                {CATEGORY_ABBR[item.category]}
              </span>
              <span className="text-[11px] leading-snug text-gray-200 font-medium">{item.label}</span>
              {!item.supported && <span className="text-[9px] text-rose-400">{t("preview.unsupported")}</span>}
            </div>
            {item.source_text && (
              <div className="text-[10px] leading-snug text-gray-500 mt-0.5 italic">{item.source_text}</div>
            )}
            <DetailPills details={item.details ?? []} badgeClass={catStyle.badge} />
          </div>
        </div>
      </div>
      {item.children?.map((child, i) => (
        <ParsedItemRow key={itemKey(child, i)} item={child} depth={(depth ?? 0) + 1} />
      ))}
    </div>
  );
}

/** Support coverage summary: progress bar + fraction */
function SupportSummary({ items }: { items: ParsedItem[] }) {
  if (items.length === 0) return null;
  const supported = items.filter((item) => item.supported).length;
  const total = items.length;
  const allSupported = supported === total;

  return (
    <div className="mt-1.5 flex items-center gap-2">
      <div className="flex-1 h-1 rounded-full bg-gray-800 overflow-hidden">
        <div
          className={`h-full rounded-full ${allSupported ? "bg-emerald-500" : "bg-amber-500"}`}
          style={{ width: `${(supported / total) * 100}%` }}
        />
      </div>
      <span className={`text-[9px] font-medium ${allSupported ? "text-emerald-400" : "text-amber-400"}`}>
        {supported}/{total}
      </span>
    </div>
  );
}

interface ParsedAbilitiesPanelProps {
  name: string;
  cardTypes?: { supertypes: string[]; core_types: string[]; subtypes: string[] } | null;
  /** Live object keywords, used to collapse a Changeling's expanded subtype
   *  list to "Changeling" in the type line (CR 702.73a). */
  keywords?: Keyword[];
  /** Localized type line from the content sidecar; preferred over formatting
   *  `cardTypes` when present (non-English locale with a translated card). */
  localizedTypeLine?: string | null;
  parseDetails: ParsedItem[] | null;
  maxHeight?: number;
}

function ParsedAbilitiesPanel({ name, cardTypes, keywords, localizedTypeLine, parseDetails, maxHeight }: ParsedAbilitiesPanelProps) {
  const { t } = useTranslation("game");
  const items = parseDetails ?? [];
  const rulings = useCardRulings(name);
  const typeLine = localizedTypeLine ?? (cardTypes ? formatTypeLine(cardTypes, keywords) : null);

  return (
    <div
      className="w-[clamp(220px,26vw,472px)] overflow-y-auto pointer-events-auto rounded-[3.5%] border border-gray-600 bg-gray-950/95 shadow-2xl backdrop-blur-sm"
      style={{ maxHeight: maxHeight ?? "80vh" }}
      data-card-hover
    >
      <div className="sticky top-0 z-10 bg-gray-950 border-b border-gray-700/80 px-3 py-2">
        <div className="flex items-center justify-between">
          <div className="text-sm font-semibold text-gray-200">{name}</div>
          <div className="text-[9px] uppercase tracking-widest text-gray-600">{t("preview.engineParse")}</div>
        </div>
        {typeLine && (
          <div className="text-[10px] text-gray-500 mt-0.5">{typeLine}</div>
        )}
        <SupportSummary items={items} />
      </div>
      <div className="px-2 py-2 space-y-0.5">
        {items.length === 0 && (
          <div className="px-1 py-2 text-xs text-gray-500 italic">{t("preview.vanilla")}</div>
        )}
        {items.map((item, i) => (
          <ParsedItemRow key={itemKey(item, i)} item={item} />
        ))}
      </div>
      {rulings.length > 0 && <RulingsSection rulings={rulings} />}
    </div>
  );
}

function CardInfoPanel({
  obj,
  altAvailable,
  activateLabels,
}: {
  obj: GameObject;
  altAvailable: boolean;
  activateLabels: string[];
}) {
  const { t } = useTranslation("game");
  const ptDisplay = computePTDisplay(obj);
  const counters = Object.entries(obj.counters).filter(([type]) => type !== "loyalty");
  const keywords = sortKeywords(obj.keywords);
  const colorsChanged =
    obj.color.length !== obj.base_color.length ||
    obj.color.some((c, i) => c !== obj.base_color[i]);
  const rulings = useCardRulings(obj.name);

  // Attribution: which permanent or transient effect granted each layered
  // characteristic. The engine writes these refs into `state.attribution`
  // during layer application; the FE only dereferences. See
  // `viewmodel/attribution.ts` for the resolution logic.
  const attribution = useGameStore((s) => s.gameState?.attribution?.[String(obj.id)]);
  const objects = useGameStore((s) => s.gameState?.objects);
  const transientContinuousEffects = useGameStore(
    (s) => s.gameState?.transient_continuous_effects,
  );
  const deref = { objects, transientContinuousEffects };
  const keywordSources = buildGrantedKeywordSources(attribution, obj.id, deref);
  const ptSources = buildPTSources(attribution, obj.id, deref);
  const chosenAttributes = obj.chosen_attributes ?? [];

  const formatChosenAttribute = (attribute: ChosenAttribute): { label: string; value: string } => {
    switch (attribute.type) {
      case "Color":
        return { label: t("preview.chosen.kind.color"), value: attribute.value };
      case "CreatureType":
        return { label: t("preview.chosen.kind.creatureType"), value: attribute.value };
      case "BasicLandType":
        return { label: t("preview.chosen.kind.basicLandType"), value: attribute.value };
      case "CardType":
        return { label: t("preview.chosen.kind.cardType"), value: attribute.value };
      case "OddOrEven":
        return { label: t("preview.chosen.kind.oddOrEven"), value: attribute.value };
      case "CardName":
        return { label: t("preview.chosen.kind.cardName"), value: attribute.value };
      case "Number":
        return { label: t("preview.chosen.kind.number"), value: String(attribute.value) };
      case "Player":
        return {
          label: t("preview.chosen.kind.player"),
          value: t("preview.chosen.playerValue", { id: attribute.value }),
        };
      case "TwoColors":
        return {
          label: t("preview.chosen.kind.twoColors"),
          value: t("preview.chosen.twoColorsValue", {
            first: attribute.value[0],
            second: attribute.value[1],
          }),
        };
      case "TributeOutcome":
        return { label: t("preview.chosen.kind.tributeOutcome"), value: attribute.value };
      case "Keyword":
        return {
          label: t("preview.chosen.kind.keyword"),
          value: getKeywordDisplayText(attribute.value),
        };
      case "Label":
        return { label: t("preview.chosen.kind.label"), value: attribute.value };
      default:
        return { label: t("preview.chosen.kind.fallback"), value: t("preview.chosen.unknown") };
    }
  };

  return (
    <div className="relative w-full border-t border-gray-600 bg-gray-900/95 px-3 py-2 text-xs text-gray-200">
      {altAvailable && (
        <div className="pointer-events-none absolute bottom-2 right-3 flex items-center gap-1.5 text-[10px] font-medium uppercase tracking-wider text-gray-300">
          <kbd className="rounded border border-gray-600 bg-gray-800 px-1.5 py-0.5 font-mono text-[10px] leading-none text-gray-200 shadow-sm">
            {t("preview.altKey")}
          </kbd>
          <span>{t("preview.parse")}</span>
          {rulings.length > 0 && (
            <span className="ml-1 rounded bg-indigo-900/70 px-1.5 py-0.5 text-[9px] font-normal normal-case tracking-normal text-indigo-200">
              {t("preview.rulingCount", { count: rulings.length })}
            </span>
          )}
        </div>
      )}
      {/* Type line */}
      <div className="font-semibold text-gray-300">
        {formatTypeLine(obj.card_types, obj.keywords)}
      </div>

      {activateLabels.length > 0 && (
        <div className="mt-1 text-cyan-300/90">
          {activateLabels.map((label) => (
            <div key={label}>{t("preview.activateCost", { cost: label })}</div>
          ))}
        </div>
      )}

      {/* Keywords */}
      {keywords.length > 0 && (
        <div className="pointer-events-auto mt-1 flex flex-wrap gap-x-2 gap-y-0.5">
          {keywords.map((kw, i) => {
            const granted = isGrantedKeyword(kw, obj.base_keywords);
            const source = keywordSources.get(getKeywordName(kw));
            const reminder = getKeywordReminderText(kw);
            const tooltipId = reminder ? `card-preview-keyword-${obj.id}-${i}` : undefined;
            return (
              <span
                key={i}
                tabIndex={reminder ? 0 : undefined}
                aria-describedby={tooltipId}
                className={`group relative cursor-default rounded-sm focus-visible:outline focus-visible:outline-1 focus-visible:outline-white/60 ${granted ? "text-indigo-300" : "text-white"}`}
              >
                {getKeywordDisplayText(kw)}
                {source && (
                  <span className="ml-1 text-[10px] text-indigo-400/80">
                    {t("preview.fromSource", { source })}
                  </span>
                )}
                {reminder && (
                  <GameplayTooltip id={tooltipId} className="right-auto left-0 mb-1.5 w-52 px-2.5 py-1.5 text-[10px] font-normal text-slate-200 shadow-xl">
                    {reminder}
                  </GameplayTooltip>
                )}
              </span>
            );
          })}
        </div>
      )}

      {/* Counters */}
      {counters.length > 0 && (
        <div className="mt-1 flex flex-wrap gap-x-3 text-gray-400">
          {counters.map(([type, count]) => (
            <span key={type}>
              {formatCounterType(type)}: {count}
            </span>
          ))}
        </div>
      )}

      {/* P/T breakdown */}
      {ptDisplay && (
        <div className="mt-1 text-gray-400">
          <span className={ptDisplay.powerColor === "green" ? "text-green-400" : ptDisplay.powerColor === "red" ? "text-red-400" : "text-white"}>
            {ptDisplay.power}
          </span>
          <span className="text-gray-500">/</span>
          <span className={ptDisplay.toughnessColor === "green" ? "text-green-400" : ptDisplay.toughnessColor === "red" ? "text-red-400" : "text-white"}>
            {ptDisplay.toughness}
          </span>
          {obj.base_power != null && obj.base_toughness != null && (
            <span className="ml-1 text-gray-500">{t("preview.basePT", { power: obj.base_power, toughness: obj.base_toughness })}</span>
          )}
          {obj.damage_marked > 0 && (
            <span className="ml-2 text-red-400">{t("preview.damage", { amount: obj.damage_marked })}</span>
          )}
          {ptSources.length > 0 && (
            <ul className="mt-0.5 ml-1 space-y-px text-[10px] text-indigo-300/90">
              {ptSources.map((c) => (
                <li key={`${c.sourceName}-${c.deltaPower}-${c.deltaToughness}`}>
                  {t("preview.ptDeltaFrom", { delta: formatPTDelta(c), source: c.sourceName })}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {/* Color changes */}
      {colorsChanged && (
        <div className="mt-1 text-gray-400">
          {t("preview.colors", { colors: obj.color.length > 0 ? obj.color.join(", ") : t("preview.colorless") })}
        </div>
      )}

      {chosenAttributes.length > 0 && (
        <div className="mt-1 text-gray-400">
          <div className="font-semibold text-gray-300">{t("preview.chosen.title")}</div>
          <div className="mt-0.5 space-y-0.5">
            {chosenAttributes.map((attribute, index) => {
              const formatted = formatChosenAttribute(attribute);
              return (
                <div key={`${attribute.type}-${index}`}>
                  {t("preview.chosen.entry", {
                    kind: formatted.label,
                    value: formatted.value,
                  })}
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

const RULINGS_INITIAL_LIMIT = 3;

function RulingsSection({ rulings }: { rulings: CardRuling[] }) {
  const { t } = useTranslation("game");
  const [expanded, setExpanded] = useState(false);

  // Sort by date descending (most recent first). React interpolation escapes all
  // text by default — never use dangerouslySetInnerHTML for ruling text.
  const sorted = [...rulings].sort((a, b) => b.date.localeCompare(a.date));
  const visible = expanded ? sorted : sorted.slice(0, RULINGS_INITIAL_LIMIT);
  const hiddenCount = sorted.length - visible.length;

  return (
    <div className="mt-3 border-t border-gray-700 px-2 pb-2 pt-2 text-xs text-gray-300">
      <div className="mb-1 font-semibold uppercase tracking-wide text-[10px] text-gray-500">
        {t("preview.rulings")}
      </div>
      <ul className="space-y-1.5">
        {visible.map((ruling, i) => (
          <li key={`${ruling.date}-${i}`} className="leading-snug">
            <span className="mr-1 text-gray-500">[{ruling.date}]</span>
            <span>{ruling.text}</span>
          </li>
        ))}
      </ul>
      {hiddenCount > 0 && (
        <button
          type="button"
          onClick={() => setExpanded(true)}
          className="mt-1.5 text-[11px] text-indigo-300 hover:text-indigo-200"
        >
          {t("preview.showMore", { count: hiddenCount })}
        </button>
      )}
      {expanded && sorted.length > RULINGS_INITIAL_LIMIT && (
        <button
          type="button"
          onClick={() => setExpanded(false)}
          className="mt-1.5 text-[11px] text-indigo-300 hover:text-indigo-200"
        >
          {t("preview.showLess")}
        </button>
      )}
    </div>
  );
}
