import type { PTColor } from "../../viewmodel/cardProps";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { computePTDisplay } from "../../viewmodel/cardProps.ts";
import { getFrameColor } from "./cardFrame.ts";

interface ArtCropCardProps {
  objectId: number;
}

const COUNTER_COLORS: Record<string, string> = {
  Plus1Plus1: "bg-green-600",
  Minus1Minus1: "bg-red-600",
  Loyalty: "bg-amber-600",
};

const PT_COLORS: Record<PTColor, string> = {
  green: "text-green-800",
  red: "text-red-700",
  white: "text-[#111]",
};

export function ArtCropCard({ objectId }: ArtCropCardProps) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  const inspectObject = useUiStore((s) => s.inspectObject);

  const cardName = obj?.name ?? "";
  const { src, isLoading } = useCardImage(cardName, { size: "art_crop" });

  if (!obj) return null;

  const hasDfc = obj.back_face != null;
  const isToken = obj.card_id === 0;
  const frameColor = getFrameColor(obj.color);
  const ptDisplay = computePTDisplay(obj);
  const counters = Object.entries(obj.counters);

  const devotionValue = (obj as any).devotion ?? null;

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

  let loyaltyClass = "text-[14px]";
  if (obj.loyalty != null) {
    const loyaltyChars = String(obj.loyalty).length;
    if (loyaltyChars >= 3) {
      loyaltyClass = "text-[11px] tracking-tighter";
    } else if (loyaltyChars >= 2) {
      loyaltyClass = "text-[13px] tracking-tight";
    }
  }

  if (isLoading || !src) {
    return (
      <div className="relative" style={{ width: "var(--art-crop-w)", height: "var(--art-crop-h)" }}>
        <div className="absolute inset-0 rounded-[6px] bg-[#151515] p-[3px] shadow-md">
          <div className="w-full h-full rounded-[4.5px] bg-[#222] animate-pulse" />
        </div>
      </div>
    );
  }

  return (
    <div className="relative drop-shadow-[0_4px_6px_rgba(0,0,0,0.6)]" style={{ width: "var(--art-crop-w)", height: "var(--art-crop-h)" }}>

      {/* 1. OUTER BLACK BORDER */}
      <div className="absolute inset-0 rounded-[6px] bg-[#151515] p-[3px] border border-black">

        {/* 2. MAIN COLORED FRAME */}
        <div
          className="w-full h-full rounded-[3px] flex flex-col relative overflow-hidden shadow-[inset_0_1px_1px_rgba(255,255,255,0.3)]"
          style={{ backgroundColor: frameColor }}
        >
          {/* Header Light Reflection Overlay */}
          <div className="absolute inset-x-0 top-0 h-[20px] bg-gradient-to-b from-white/40 to-transparent pointer-events-none z-10" />

          {/* 3. HEADER AREA: Uses isToken to make the background slightly translucent for tokens */}
          <div className={`h-[20px] w-full flex items-center px-1.5 shrink-0 z-10 border-b border-black/40 shadow-[0_1px_2px_rgba(0,0,0,0.4)] ${isToken ? 'bg-black/10' : ''}`}>
            <span className={`text-[11.5px] font-extrabold tracking-tight leading-none truncate mt-[1px] ${isToken ? 'text-[#1a1a1a] drop-shadow-[0_1px_1px_rgba(255,255,255,0.6)]' : 'text-[#111] drop-shadow-[0_1px_0_rgba(255,255,255,0.5)]'}`}>
              {cardName}
            </span>
          </div>

          {/* 4. ART AREA */}
          <div className="flex-1 w-full px-[2px] pb-[2px] flex flex-col relative z-0">
            <div className="w-full h-full relative rounded-[1.5px] overflow-hidden border border-black/80 shadow-[inset_0_1px_3px_rgba(0,0,0,0.6)] bg-black">
              <img
                src={src}
                alt={cardName}
                className="absolute inset-0 w-full h-full object-cover"
                draggable={false}
              />

              <div className="absolute inset-x-0 bottom-0 h-6 bg-gradient-to-t from-black/50 to-transparent pointer-events-none" />

              {counters.length > 0 && (
                <div className="absolute top-1 right-1 z-20 flex flex-col gap-0.5">
                  {counters.map(([type, count]) => (
                    <div
                      key={type}
                      className={`rounded-full w-5 h-5 flex items-center justify-center text-[9px] font-bold text-white shadow-md border border-black/50 ${COUNTER_COLORS[type] ?? "bg-purple-600"}`}
                    >
                      {count}
                    </div>
                  ))}
                </div>
              )}

              {hasDfc && (
                <button
                  type="button"
                  className="absolute bottom-1 left-4 z-20 bg-gray-900/90 border border-gray-500 rounded-sm px-1 py-0.5 text-[8px] font-bold text-gray-300 hover:bg-gray-700 hover:text-white cursor-pointer shadow-md"
                  onMouseEnter={() => inspectObject(objectId, 1)}
                  onMouseLeave={() => inspectObject(objectId, 0)}
                >
                  DFC
                </button>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* 5. GOLDEN DEVOTION/TRACKER BADGE: Hidden if it's a token */}
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
          <div className="rounded-[6px] bg-gradient-to-b from-[#e2e4e6] to-[#888c91] p-[2px] shadow-[inset_0_1px_1px_rgba(255,255,255,0.9),inset_0_-1px_1px_rgba(0,0,0,0.5),0_2px_4px_rgba(0,0,0,0.8)] border border-black/80">
            <div className="bg-[#f0f2f5] rounded-[6px] px-2 py-[1px] min-w-[2.75rem] flex justify-center items-baseline shadow-[inset_0_2px_4px_rgba(0,0,0,0.4),inset_0_1px_2px_rgba(0,0,0,0.6),0_1px_0_rgba(255,255,255,0.4)]">
              <span className={`font-serif font-black leading-none ${ptNumClass} ${PT_COLORS[ptDisplay.powerColor] || "text-[#111]"}`}>
                {ptDisplay.power}
              </span>
              <span className={`font-serif font-bold text-[#666] leading-none mx-[1px] ${ptSlashClass}`}>
                /
              </span>
              <span className={`font-serif font-black leading-none ${ptNumClass} ${PT_COLORS[ptDisplay.toughnessColor] || "text-[#111]"}`}>
                {ptDisplay.toughness}
              </span>
            </div>
          </div>
        </div>
      )}

      {/* Floating loyalty */}
      {obj.loyalty != null && (
        <div className="absolute -bottom-[3px] -right-[3px] z-20">
          <div className="rounded-full bg-gradient-to-b from-[#e2e4e6] to-[#888c91] p-[2px] shadow-[inset_0_1px_1px_rgba(255,255,255,0.9),inset_0_-1px_1px_rgba(0,0,0,0.5),0_2px_4px_rgba(0,0,0,0.8)] border border-black/80">
            <div className="bg-gray-800 border-[1px] border-amber-600/50 rounded-full px-2.5 py-[1px] min-w-[2.75rem] flex justify-center items-center shadow-[inset_0_2px_4px_rgba(0,0,0,0.8),inset_0_1px_2px_rgba(0,0,0,0.9),0_1px_0_rgba(255,255,255,0.2)]">
              <span className={`font-bold text-amber-400 leading-none ${loyaltyClass}`}>
                {obj.loyalty}
              </span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}