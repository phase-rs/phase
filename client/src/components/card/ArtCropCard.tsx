import { memo, useEffect, useMemo, useState, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";

import type { PTColor } from "../../viewmodel/cardProps";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useIsMobile } from "../../hooks/useIsMobile.ts";
import { cardImageLookup, tokenFiltersForObject } from "../../services/cardImageLookup.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { COUNTER_COLORS, computePTDisplay, toRoman } from "../../viewmodel/cardProps.ts";
import { CounterTooltip } from "../ui/CounterTooltip.tsx";
import { LoyaltyBadge } from "../ui/LoyaltyBadge.tsx";
import { CardArtFallback } from "./CardArtFallback.tsx";
import { frameNeedsLightText, getCardDisplayColors, getFrameGradient } from "./cardFrame.ts";

interface ArtCropCardProps {
  objectId: number;
}

const PT_COLORS: Record<PTColor, string> = {
  green: "text-green-800",
  red: "text-red-700",
  white: "text-[#111]",
};

// Stable empty ref so the unbounded-counter selector returns the same value when
// absent, avoiding a spurious re-render every state tick (mirrors PermanentCard).
const EMPTY_UNBOUNDED_COUNTERS: string[] = [];

export const ArtCropCard = memo(function ArtCropCard({ objectId }: ArtCropCardProps) {
  const { t } = useTranslation("game");
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  // CR 732.2a / CR 701.34a: the counter-type keys the engine marks as ∞ (unbounded
  // counter-growth loop) for this object. Same channel PermanentCard's full-card pill
  // reads — both battlefield display modes must agree, or the ∞ silently drops in one.
  const unboundedCounterTypes = useGameStore(
    (s) => s.gameState?.derived?.unbounded_counters?.[String(objectId)] ?? EMPTY_UNBOUNDED_COUNTERS,
  );
  const isMobile = useIsMobile();
  const inspectObject = useUiStore((s) => s.inspectObject);
  const isCompactHeight = useIsCompactHeight();
  const controllerIdentity = useGameStore(
    (s) => obj && s.gameState?.players?.find((p) => p.id === obj.controller)?.commander_color_identity,
  );

  const cardName = obj?.face_down ? t("card.faceDownName") : (obj?.name ?? "");
  const imageLookup = obj
    ? cardImageLookup(obj)
    : { name: "", faceIndex: 0, oracleId: undefined, faceName: undefined };
  const isToken = obj?.display_source === "Token";
  const { src: cardSrc, isLoading: cardLoading } = useCardImage(obj?.face_down ? "" : imageLookup.name, {
    size: "art_crop",
    faceIndex: imageLookup.faceIndex,
    isToken: obj?.face_down ? false : isToken,
    tokenFilters: !obj?.face_down && isToken && obj ? tokenFiltersForObject(obj) : undefined,
    tokenImageRef: !obj?.face_down && isToken && obj ? obj.token_image_ref : undefined,
    oracleId: obj?.face_down ? undefined : imageLookup.oracleId,
    faceName: obj?.face_down ? undefined : imageLookup.faceName,
  });

  // A resolved URL can still 404 (future-dated sets not yet on the CDN, stale
  // token image refs). Without this the default battlefield renderer would show
  // the browser's broken-image glyph — the same visual defect issue #6156
  // reports — while `CardImage` recovered. Reset on src change so a new face
  // re-tries; mirrors `CardImage.tsx`.
  const [artError, setArtError] = useState(false);
  useEffect(() => setArtError(false), [cardSrc]);

  const { frameGradient, lightText, ptDisplay } = useMemo(() => {
    if (!obj) return { frameGradient: "", lightText: false, ptDisplay: null };
    const isLand = obj.card_types.core_types.includes("Land");
    const dc = getCardDisplayColors(obj.color, isLand, obj.card_types.subtypes, obj.available_mana_pips, controllerIdentity || undefined);
    return {
      frameGradient: getFrameGradient(dc),
      lightText: frameNeedsLightText(dc),
      ptDisplay: computePTDisplay(obj),
    };
  }, [obj, controllerIdentity]);

  if (!obj) return null;

  const src = obj.face_down ? CARD_BACK_URL : cardSrc;
  const isLoading = obj.face_down ? false : cardLoading;
  const hasDfc = !obj.face_down && obj.back_face != null;
  // Filter out loyalty counters — shown separately as the loyalty badge
  const counters = Object.entries(obj.counters).filter((entry): entry is [string, number] => entry[1] != null && entry[0] !== "loyalty");
  const devotionValue = obj.devotion ?? null;
  // --- Dynamic Text Sizing Logic ---
  let ptNumClass = "text-[14px]";
  let ptSlashClass = "text-[13px]";

  if (ptDisplay) {
    const totalChars = String(ptDisplay.power).length + String(ptDisplay.toughness).length;
    if (totalChars >= 6) {
      ptNumClass = "text-[10px] tracking-tighter";
      ptSlashClass = "text-[10px]";
    } else if (totalChars >= 4) {
      ptNumClass = "text-[12px] tracking-tight";
      ptSlashClass = "text-[11px]";
    }
  }

  // Genuinely still resolving art — pulse until the async lookup settles.
  if (!obj.face_down && isLoading) {
    return (
      <div className="relative" style={{ width: "var(--art-crop-w)", height: "var(--art-crop-h)" }}>
        <div className="absolute inset-0 rounded-[6px] bg-[#151515] p-[3px] shadow-md">
          <div className="w-full h-full rounded-[4.5px] bg-[#222] animate-pulse" />
        </div>
      </div>
    );
  }

  const renderedSrc = obj.face_down ? CARD_BACK_URL : (src ?? "");
  const headerHeight = isCompactHeight
    ? "clamp(8px, calc(var(--art-crop-h) * 0.16), 12px)"
    : "clamp(8px, calc(var(--art-crop-h) * 0.18), 20px)";
  const headerInlinePadding = "clamp(3px, calc(var(--art-crop-w) * 0.045), 6px)";
  const headerStyle = {
    height: headerHeight,
    paddingInline: headerInlinePadding,
  } as CSSProperties;
  const headerTextStyle = {
    fontSize: isCompactHeight
      ? "clamp(6.5px, calc(var(--art-crop-h) * 0.105), 8px)"
      : "clamp(6.5px, calc(var(--art-crop-h) * 0.105), 11.5px)",
  } as CSSProperties;
  const counterStyle = {
    width: "clamp(13px, calc(var(--art-crop-w) * 0.24), 20px)",
    height: "clamp(13px, calc(var(--art-crop-w) * 0.24), 20px)",
    fontSize: "clamp(6.5px, calc(var(--art-crop-h) * 0.085), 9px)",
  } as CSSProperties;
  const ptOuterStyle = {
    padding: "clamp(1px, calc(var(--art-crop-h) * 0.018), 2px)",
  } as CSSProperties;
  const ptInnerStyle = {
    minWidth: "clamp(1.55rem, calc(var(--art-crop-w) * 0.42), 2.75rem)",
    paddingInline: "clamp(4px, calc(var(--art-crop-w) * 0.075), 8px)",
    paddingBlock: "clamp(0px, calc(var(--art-crop-h) * 0.012), 1px)",
  } as CSSProperties;
  const ptNumStyle = {
    fontSize: "clamp(9px, calc(var(--art-crop-h) * 0.145), 14px)",
  } as CSSProperties;
  const ptSlashStyle = {
    fontSize: "clamp(8px, calc(var(--art-crop-h) * 0.13), 13px)",
  } as CSSProperties;

  return (
    <div className="relative select-none drop-shadow-[0_4px_6px_rgba(0,0,0,0.6)]" style={{ width: "var(--art-crop-w)", height: "var(--art-crop-h)" }}>

      {/* 1. OUTER BLACK BORDER */}
      <div className="absolute inset-0 rounded-[6px] bg-[#151515] p-[3px] border border-black">

        {/* 2. MAIN COLORED FRAME */}
        <div
          className="w-full h-full rounded-[3px] flex flex-col relative overflow-hidden shadow-[inset_0_1px_1px_rgba(255,255,255,0.3)]"
          style={{ background: frameGradient }}
        >
          {/* Header Light Reflection Overlay */}
          <div
            className="absolute inset-x-0 top-0 bg-gradient-to-b from-white/40 to-transparent pointer-events-none z-10"
            style={{ height: headerHeight }}
          />

          {/* 3. HEADER AREA: Uses isToken to make the background slightly translucent for tokens */}
          <div
            className={`w-full flex items-center shrink-0 z-10 border-b border-black/40 shadow-[0_1px_2px_rgba(0,0,0,0.4)] ${isToken ? 'bg-black/10' : ''}`}
            style={headerStyle}
          >
            <span
              className={`font-extrabold tracking-tight leading-none truncate mt-[1px] ${lightText ? 'text-white drop-shadow-[0_1px_1px_rgba(0,0,0,0.8)]' : isToken ? 'text-[#1a1a1a] drop-shadow-[0_1px_1px_rgba(255,255,255,0.6)]' : 'text-[#111] drop-shadow-[0_1px_0_rgba(255,255,255,0.5)]'}`}
              style={headerTextStyle}
            >
              {cardName}
            </span>
          </div>

          {/* 4. ART AREA */}
          <div className="flex-1 w-full px-[2px] pb-[2px] flex flex-col relative z-0">
            <div className="w-full h-full relative rounded-[1.5px] overflow-hidden border border-black/80 shadow-[inset_0_1px_3px_rgba(0,0,0,0.6)] bg-black">
              {/* Issue #6156: a token with no official paper printing (Kibo,
                  Uktabi Prince's Banana) resolves to a null src, as does any
                  card whose art fetch is rejected. Swapping only the art —
                  rather than returning a bare tile before the frame — keeps the
                  header name, P/T box, counters and loyalty badge on screen, so
                  an artless permanent loses its picture but never its game
                  state. `src` is non-null for face-down cards (CARD_BACK_URL),
                  so those still render the card back here. */}
              {src && !artError ? (
                <img
                  src={renderedSrc}
                  alt={cardName}
                  draggable={false}
                  onError={() => setArtError(true)}
                  className="absolute inset-0 w-full h-full object-cover"
                />
              ) : (
                <CardArtFallback name={cardName} variant="artCrop" className="absolute inset-0 w-full h-full" />
              )}

              <div className="absolute inset-x-0 bottom-0 h-6 bg-gradient-to-t from-black/50 to-transparent pointer-events-none" />

              {/* Keyword badges are rendered by the parent PermanentCard at its
                  overflow-visible level (so they can straddle the card edge),
                  covering both art-crop and full-card modes. */}

              {/* Top-right overlay stack: counter badges kept clear of the
                  bottom P/T and loyalty badges. */}
              <div className="absolute top-0.5 right-0.5 z-[60] flex flex-col items-end gap-0.5">
                {counters.map(([type, count]) => {
                  // CR 732.2a / CR 701.34a: an accepted counter-growth loop pumps this
                  // counter unboundedly — render ∞ instead of the (still-finite) real count.
                  const isUnbounded = unboundedCounterTypes.includes(type);
                  return (
                    <CounterTooltip
                      key={type}
                      type={type}
                      count={count}
                      isUnbounded={isUnbounded}
                    >
                      <span
                        className={`rounded-full flex items-center justify-center font-bold text-white shadow-md border border-black/50 ${COUNTER_COLORS[type] ?? "bg-purple-600"}`}
                        style={counterStyle}
                      >
                        {isUnbounded ? "∞" : count}
                      </span>
                    </CounterTooltip>
                  );
                })}
              </div>

              {hasDfc && (
                <button
                  type="button"
                  className="absolute bottom-1 left-4 z-30 bg-gray-900/90 border border-gray-500 rounded-sm px-1 py-0.5 text-[8px] font-bold text-gray-300 hover:bg-gray-700 hover:text-white cursor-pointer shadow-md"
                  onMouseEnter={isMobile ? undefined : () => inspectObject(objectId, 1)}
                  onMouseLeave={isMobile ? undefined : () => inspectObject(objectId, 0)}
                >
                  {t("card.dfc")}
                </button>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* 5a. CLASS LEVEL BADGE (CR 716) — gold-leaf bookmark */}
      {obj.class_level != null && (
        <div className="absolute -bottom-[3px] -left-[3px] z-20 flex items-center justify-center">
          <div className="rounded-t-[3px] rounded-b-none bg-gradient-to-b from-amber-950 to-stone-900 px-1.5 pt-[3px] pb-[5px] border border-amber-800/60 shadow-[inset_0_1px_1px_rgba(255,255,255,0.15),0_2px_4px_rgba(0,0,0,0.8)] clip-bookmark">
            <span className="font-serif font-bold text-amber-300 text-[10px] leading-none drop-shadow-[0_1px_1px_rgba(0,0,0,0.8)]">
              {toRoman(obj.class_level)}
            </span>
          </div>
        </div>
      )}

      {/* 5b. GOLDEN DEVOTION/TRACKER BADGE */}
      {!isToken && devotionValue != null && (
        <div className="absolute -bottom-[2px] -left-[2px] z-20 flex items-center justify-center">
          <div className="w-[18px] h-[18px] rounded-[2px] bg-gradient-to-br from-[#f2cc59] to-[#c78b1e] border border-[#4a350d] flex items-center justify-center shadow-[inset_0_1px_1px_rgba(255,255,255,0.7),inset_0_-1px_1px_rgba(0,0,0,0.3),0_2px_4px_rgba(0,0,0,0.8)]">
             <span className="font-bold text-[#1a1304] text-[12px] leading-none drop-shadow-[0_1px_0_rgba(255,255,255,0.3)] mt-[1px]">
               {devotionValue}
             </span>
          </div>
        </div>
      )}

      {/* 6. P/T BOX */}
      {ptDisplay && (
        <div className="absolute -bottom-[3px] -right-[3px] z-20">
          <div
            className="rounded-[6px] bg-gradient-to-b from-[#e2e4e6] to-[#888c91] shadow-[inset_0_1px_1px_rgba(255,255,255,0.9),inset_0_-1px_1px_rgba(0,0,0,0.5),0_2px_4px_rgba(0,0,0,0.8)] border border-black/80"
            style={ptOuterStyle}
          >
            <div
              className="bg-[#f0f2f5] rounded-[6px] flex justify-center items-baseline shadow-[inset_0_2px_4px_rgba(0,0,0,0.4),inset_0_1px_2px_rgba(0,0,0,0.6),0_1px_0_rgba(255,255,255,0.4)]"
              style={ptInnerStyle}
            >
              <span
                className={`font-serif font-black leading-none ${ptNumClass} ${PT_COLORS[ptDisplay.powerColor] || "text-[#111]"}`}
                style={ptNumStyle}
              >
                {ptDisplay.power}
              </span>
              <span
                className={`font-serif font-bold text-[#666] leading-none mx-[1px] ${ptSlashClass}`}
                style={ptSlashStyle}
              >
                /
              </span>
              <span
                className={`font-serif font-black leading-none ${ptNumClass} ${PT_COLORS[ptDisplay.toughnessColor] || "text-[#111]"}`}
                style={ptNumStyle}
              >
                {ptDisplay.toughness}
              </span>
            </div>
          </div>
        </div>
      )}

      {obj.loyalty != null && (
        <LoyaltyBadge
          amount={obj.loyalty}
          kind="total"
          size="battlefield"
          reinforcedTopRim
          className="absolute z-30"
          style={{
            position: "absolute",
            fontSize: "clamp(13px, calc(var(--art-crop-h) * 0.19), 22px)",
            bottom: "-5px",
            ...(ptDisplay ? { left: "-5px" } : { right: "-5px" }),
          }}
        />
      )}
    </div>
  );
});
