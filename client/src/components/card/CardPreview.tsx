import { useEffect, useState } from "react";

import type { GameObject } from "../../adapter/types.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useEngineCardData, useCardParseDetails, type ParsedItem } from "../../hooks/useEngineCardData.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { ManaCostPips } from "../mana/ManaCostPips.tsx";
import { computePTDisplay, formatCounterType, formatTypeLine, toRoman } from "../../viewmodel/cardProps.ts";
import {
  getKeywordDisplayText,
  isGrantedKeyword,
  sortKeywords,
} from "../../viewmodel/keywordProps.ts";

interface CardPreviewProps {
  cardName: string | null;
  backFaceName?: string | null;
  faceIndex?: number;
  position?: { x: number; y: number };
}

export function CardPreview({
  cardName,
  backFaceName,
  faceIndex,
  position,
}: CardPreviewProps) {
  if (!cardName) return null;

  return (
    <CardPreviewInner
      cardName={cardName}
      backFaceName={backFaceName ?? null}
      faceIndex={faceIndex}
      position={position}
    />
  );
}

function CardPreviewInner({
  cardName,
  backFaceName: backFaceNameProp,
  faceIndex,
  position,
}: {
  cardName: string;
  backFaceName: string | null;
  faceIndex?: number;
  position?: { x: number; y: number };
}) {
  const inspectedObjectId = useUiStore((s) => s.inspectedObjectId);
  const inspectObject = useUiStore((s) => s.inspectObject);
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

  const isToken = obj?.card_id === 0;
  const { src, isLoading } = useCardImage(cardName, {
    size: "normal",
    faceIndex,
    isToken,
    tokenFilters: isToken ? { power: obj?.power, toughness: obj?.toughness, colors: obj?.color } : undefined,
  });
  const classLevel = obj?.class_level;
  const [pointerPosition, setPointerPosition] = useState<{ x: number; y: number } | null>(null);
  const altHeld = useUiStore((s) => s.altHeld);
  const [ctrlHeld, setCtrlHeld] = useState(false);
  const [isMobile, setIsMobile] = useState(
    typeof window !== "undefined" && window.innerWidth < 1024,
  );

  useEffect(() => {
    function handleResize() {
      setIsMobile(window.innerWidth < 1024);
    }
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

  useEffect(() => {
    if (typeof window === "undefined") return undefined;

    function handlePointerMove(event: MouseEvent) {
      setPointerPosition({ x: event.clientX, y: event.clientY });
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Control") setCtrlHeld(true);
    }

    function handleKeyUp(event: KeyboardEvent) {
      if (event.key === "Control") setCtrlHeld(false);
    }

    window.addEventListener("mousemove", handlePointerMove);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("keyup", handleKeyUp);
    return () => {
      window.removeEventListener("mousemove", handlePointerMove);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("keyup", handleKeyUp);
    };
  }, []);

  // On desktop, Ctrl swaps to the back face
  const showBackFace = !isMobile && ctrlHeld && backFaceName != null;
  // Fetch back face image when Ctrl is held (hook must always be called, but with empty
  // string when not needed so useCardImage short-circuits without a network request)
  const backFaceImgResult = useCardImage(showBackFace ? backFaceName! : "", {
    size: "normal",
    faceIndex: 1,
  });

  // Mobile overlay mode: centered with backdrop
  if (isMobile) {
    return (
      <MobilePreviewOverlay
        cardName={cardName}
        backFaceName={backFaceName}
        faceIndex={faceIndex}
        obj={obj}
        onDismiss={() => inspectObject(null)}
      />
    );
  }

  // Desktop mode: cursor-following or fixed position
  const activeSrc = showBackFace ? backFaceImgResult.src : src;
  const activeLoading = showBackFace ? backFaceImgResult.isLoading : isLoading;
  const displayName = showBackFace ? backFaceName! : cardName;
  const showInfoPanel = obj?.zone === "Battlefield";
  const infoPanelHeight = showInfoPanel ? 120 : 0;
  const previewWidth =
    typeof window === "undefined" ? 472 : Math.min(Math.max(window.innerWidth * 0.26, 220), 472);
  const previewHeight =
    (typeof window === "undefined"
      ? 661
      : Math.min(window.innerHeight * 0.8, previewWidth * (7 / 5))) + infoPanelHeight;
  const viewportWidth = typeof window === "undefined" ? 1440 : window.innerWidth;
  const viewportHeight = typeof window === "undefined" ? 900 : window.innerHeight;
  const gap = 20;

  const cursorStyle: React.CSSProperties | null = pointerPosition
    ? {
        left:
          pointerPosition.x > viewportWidth / 2
            ? Math.max(16, pointerPosition.x - previewWidth - gap)
            : Math.min(pointerPosition.x + gap, viewportWidth - previewWidth - 16),
      }
    : null;

  const margin = 16;

  const style: React.CSSProperties = position
    ? {
        left: Math.min(position.x + 16, window.innerWidth - 488),
        top: Math.min(position.y - 200, window.innerHeight - 736),
      }
    : cursorStyle
      ? altHeld
        ? { ...cursorStyle, top: margin }
        : {
            ...cursorStyle,
            top: Math.min(
              Math.max(margin, pointerPosition!.y - previewHeight / 2),
              viewportHeight - previewHeight - margin,
            ),
          }
    : {
        right: "calc(env(safe-area-inset-right) + 1rem + var(--game-right-rail-offset, 0px))",
        top: "calc(env(safe-area-inset-top) + var(--game-top-overlay-offset, 0px) + 1rem)",
      };

  return (
    <div
      className="fixed z-[100] pointer-events-none"
      style={style}
      data-card-preview
    >
      {altHeld && (frontParseDetails || engineFrontFace) ? (
        <ParsedAbilitiesPanel
          name={showBackFace ? (engineBackFace?.name ?? backFaceName ?? "") : (obj?.name ?? engineFrontFace?.name ?? frontFaceName)}
          cardTypes={showBackFace ? engineBackFace?.card_type : (obj?.card_types ?? engineFrontFace?.card_type)}
          parseDetails={showBackFace && backParseDetails ? backParseDetails : frontParseDetails}
          maxHeight={viewportHeight - margin * 2}
        />
      ) : (
        <CardImagePreview
          cardName={displayName}
          classLevel={classLevel}
          showInfoPanel={showInfoPanel}
          obj={obj}
          isLoading={activeLoading}
          src={activeSrc}
          backFaceHint={backFaceName != null && !showBackFace ? "Hold Ctrl for back face" : null}
        />
      )}
    </div>
  );
}

/** Mobile/tablet: centered overlay with backdrop dismiss and side-by-side DFCs */
function MobilePreviewOverlay({
  cardName,
  backFaceName,
  faceIndex,
  obj,
  onDismiss,
}: {
  cardName: string;
  backFaceName: string | null;
  faceIndex?: number;
  obj: GameObject | null;
  onDismiss: () => void;
}) {
  const { src, isLoading } = useCardImage(cardName, { size: "normal", faceIndex });
  const showInfoPanel = obj?.zone === "Battlefield";
  const classLevel = obj?.class_level;
  const hasDualFace = backFaceName != null;

  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center bg-black/60 backdrop-blur-sm"
      data-card-preview
      onClick={(e) => {
        // Dismiss when tapping the backdrop (not the card images)
        if (e.target === e.currentTarget) onDismiss();
      }}
    >
      <div className={`flex ${hasDualFace ? "gap-3" : ""} items-start max-h-[85vh] max-w-[95vw]`}>
        {/* Front face */}
        <div className="shrink-0">
          <CardImagePreview
            cardName={cardName}
            classLevel={classLevel}
            showInfoPanel={showInfoPanel}
            obj={obj}
            isLoading={isLoading}
            src={src}
            backFaceHint={null}
            mobileMode
          />
        </div>

        {/* Back face — shown side by side on mobile for DFCs */}
        {hasDualFace && (
          <div className="shrink-0">
            <BackFaceImage cardName={backFaceName!} mobileMode />
          </div>
        )}
      </div>

      {/* Dismiss hint */}
      <div className="absolute bottom-6 left-1/2 -translate-x-1/2 rounded-full bg-black/60 px-4 py-1.5 text-xs text-gray-300">
        Tap outside to dismiss
      </div>
    </div>
  );
}

/** Renders the back face image for side-by-side DFC display */
function BackFaceImage({ cardName, mobileMode }: { cardName: string; mobileMode?: boolean }) {
  const { src, isLoading } = useCardImage(cardName, { size: "normal", faceIndex: 1 });

  const sizeClass = mobileMode
    ? "max-h-[75vh] w-[40vw] max-w-[300px]"
    : "max-h-[80vh] max-w-[42vw] w-[clamp(220px,26vw,472px)] md:max-w-[45vw]";

  if (isLoading || !src) {
    return (
      <div className={`${sizeClass} aspect-[5/7] rounded-[4%] border border-gray-600 bg-gray-700 shadow-2xl animate-pulse`} />
    );
  }

  return (
    <div className="border border-gray-600 overflow-hidden shadow-2xl rounded-[4%]">
      <img
        src={src}
        alt={cardName}
        className={`${sizeClass} object-cover`}
        draggable={false}
      />
    </div>
  );
}

/** Shared card image preview used by both desktop and mobile modes */
function CardImagePreview({
  cardName,
  classLevel,
  showInfoPanel,
  obj,
  isLoading,
  src,
  backFaceHint,
  mobileMode,
}: {
  cardName: string;
  classLevel?: number | null;
  showInfoPanel?: boolean;
  obj: GameObject | null;
  isLoading: boolean;
  src: string | null;
  backFaceHint: string | null;
  mobileMode?: boolean;
}) {
  const sizeClass = mobileMode
    ? "max-h-[75vh] w-[40vw] max-w-[300px]"
    : "max-h-[80vh] max-w-[42vw] w-[clamp(220px,26vw,472px)] md:max-w-[45vw]";

  if (isLoading || !src) {
    return (
      <div className={`${sizeClass} aspect-[5/7] rounded-[4%] border border-gray-600 bg-gray-700 shadow-2xl animate-pulse`} />
    );
  }

  return (
    <div className={`${sizeClass} border border-gray-600 overflow-hidden shadow-2xl ${showInfoPanel ? "rounded-t-[4%] rounded-b-lg bg-gray-900" : "rounded-[4%]"}`}>
      <div className="relative rounded-[4%] overflow-hidden">
        <img
          src={src}
          alt={cardName}
          className={`${sizeClass} object-cover`}
          draggable={false}
        />
        {obj?.mana_cost && (
          <ManaCostPips cost={obj.mana_cost} size="lg" className="absolute right-[7.00%] top-[5.25%] z-10" />
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
      </div>
      {showInfoPanel && obj && <CardInfoPanel obj={obj} />}
      {backFaceHint && (
        <div className="bg-gray-900/80 text-center py-1 text-[10px] text-gray-400">{backFaceHint}</div>
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
              {!item.supported && <span className="text-[9px] text-rose-400">unsupported</span>}
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
  parseDetails: ParsedItem[] | null;
  maxHeight?: number;
}

function ParsedAbilitiesPanel({ name, cardTypes, parseDetails, maxHeight }: ParsedAbilitiesPanelProps) {
  const items = parseDetails ?? [];

  return (
    <div
      className="w-[clamp(220px,26vw,472px)] overflow-y-auto pointer-events-auto rounded-[3.5%] border border-gray-600 bg-gray-950/95 shadow-2xl backdrop-blur-sm"
      style={{ maxHeight: maxHeight ?? "80vh" }}
      data-card-hover
    >
      <div className="sticky top-0 z-10 bg-gray-950 border-b border-gray-700/80 px-3 py-2">
        <div className="flex items-center justify-between">
          <div className="text-sm font-semibold text-gray-200">{name}</div>
          <div className="text-[9px] uppercase tracking-widest text-gray-600">Engine Parse</div>
        </div>
        {cardTypes && formatTypeLine(cardTypes) && (
          <div className="text-[10px] text-gray-500 mt-0.5">{formatTypeLine(cardTypes)}</div>
        )}
        <SupportSummary items={items} />
      </div>
      <div className="px-2 py-2 space-y-0.5">
        {items.length === 0 && (
          <div className="px-1 py-2 text-xs text-gray-500 italic">Vanilla — no parsed abilities</div>
        )}
        {items.map((item, i) => (
          <ParsedItemRow key={itemKey(item, i)} item={item} />
        ))}
      </div>
    </div>
  );
}

function CardInfoPanel({ obj }: { obj: GameObject }) {
  const ptDisplay = computePTDisplay(obj);
  const counters = Object.entries(obj.counters).filter(([type]) => type !== "Loyalty");
  const keywords = sortKeywords(obj.keywords);
  const colorsChanged =
    obj.color.length !== obj.base_color.length ||
    obj.color.some((c, i) => c !== obj.base_color[i]);

  return (
    <div className="w-full border-t border-gray-600 bg-gray-900/95 px-3 py-2 text-xs text-gray-200">
      {/* Type line */}
      <div className="font-semibold text-gray-300">
        {formatTypeLine(obj.card_types)}
      </div>

      {/* Keywords */}
      {keywords.length > 0 && (
        <div className="mt-1 flex flex-wrap gap-x-2 gap-y-0.5">
          {keywords.map((kw, i) => (
            <span
              key={i}
              className={isGrantedKeyword(kw, obj.base_keywords) ? "text-indigo-300" : "text-white"}
            >
              {getKeywordDisplayText(kw)}
            </span>
          ))}
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
            <span className="ml-1 text-gray-500">(base {obj.base_power}/{obj.base_toughness})</span>
          )}
          {obj.damage_marked > 0 && (
            <span className="ml-2 text-red-400">Damage: {obj.damage_marked}</span>
          )}
        </div>
      )}

      {/* Color changes */}
      {colorsChanged && (
        <div className="mt-1 text-gray-400">
          Colors: {obj.color.length > 0 ? obj.color.join(", ") : "Colorless"}
        </div>
      )}
    </div>
  );
}
