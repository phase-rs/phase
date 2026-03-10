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

  if (isLoading || !src) {
    return (
      <div className="relative mt-2" style={{ width: "var(--art-crop-w)", height: "var(--art-crop-h)" }}>
        <div className="absolute inset-0 rounded-[6px] bg-[#151515] p-[3px] shadow-md">
          <div className="w-full h-full rounded-[4.5px] bg-[#222] animate-pulse" />
        </div>
      </div>
    );
  }

  return (
    <div className="relative mt-2 drop-shadow-[0_4px_6px_rgba(0,0,0,0.6)]" style={{ width: "var(--art-crop-w)", height: "var(--art-crop-h)" }}>

      {/* 1. OUTER BLACK BORDER */}
      <div className="absolute inset-0 rounded-[6px] bg-[#151515] p-[3px] border border-black">

        {/* 2. MAIN COLORED FRAME */}
        <div
          className="w-full h-full rounded-[3px] flex flex-col relative overflow-hidden shadow-[inset_0_1px_1px_rgba(255,255,255,0.3)]"
          style={{ backgroundColor: frameColor }}
        >
          {/* Header Light Reflection Overlay */}
          <div className="absolute inset-x-0 top-0 h-[20px] bg-gradient-to-b from-white/40 to-transparent pointer-events-none z-10" />

          {/* 3. HEADER AREA */}
          <div className="h-[20px] w-full flex items-center px-1.5 shrink-0 z-10 border-b border-black/40 shadow-[0_1px_2px_rgba(0,0,0,0.4)]">
            <span className="text-[11.5px] font-extrabold text-[#111] tracking-tight leading-none truncate drop-shadow-[0_1px_0_rgba(255,255,255,0.5)] mt-[1px]">
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

              {/* Bottom shadow gradient */}
              <div className="absolute inset-x-0 bottom-0 h-6 bg-gradient-to-t from-black/50 to-transparent pointer-events-none" />

              {/* Counter badges (Top Right) */}
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

              {/* DFC indicator */}
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

      {/* 5. GOLDEN DEVOTION/TRACKER BADGE */}
      {devotionValue != null && (
        <div className="absolute -bottom-[2px] -left-[2px] z-20 flex items-center justify-center">
          <div className="w-[18px] h-[18px] rounded-[2px] bg-gradient-to-br from-[#f2cc59] to-[#c78b1e] border border-[#4a350d] flex items-center justify-center shadow-[inset_0_1px_1px_rgba(255,255,255,0.7),inset_0_-1px_1px_rgba(0,0,0,0.3),0_2px_4px_rgba(0,0,0,0.8)]">
             <span className="font-bold text-[#1a1304] text-[12px] leading-none drop-shadow-[0_1px_0_rgba(255,255,255,0.3)] mt-[1px]">
               {devotionValue}
             </span>
          </div>
        </div>
      )}

      {/* 6. P/T BOX: Inner pill now strictly uses dark inset shadows to appear recessed */}
      {ptDisplay && (
        <div className="absolute -bottom-[3px] -right-[3px] z-20">
          <div className="rounded-full bg-gradient-to-b from-[#e2e4e6] to-[#888c91] p-[2px] shadow-[inset_0_1px_1px_rgba(255,255,255,0.9),inset_0_-1px_1px_rgba(0,0,0,0.5),0_2px_4px_rgba(0,0,0,0.8)] border border-black/80">
            {/* The inset inner pill */}
            <div className="bg-[#f0f2f5] rounded-full px-2.5 py-[1px] min-w-[2.75rem] flex justify-center items-center shadow-[inset_0_2px_4px_rgba(0,0,0,0.4),inset_0_1px_2px_rgba(0,0,0,0.6),0_1px_0_rgba(255,255,255,0.4)]">
              <span className={`font-serif font-black text-[14px] leading-none ${PT_COLORS[ptDisplay.powerColor] || "text-[#111]"}`}>
                {ptDisplay.power}
              </span>
              <span className="font-serif font-bold text-[#666] text-[13px] leading-none mx-[1px]">/</span>
              <span className={`font-serif font-black text-[14px] leading-none ${PT_COLORS[ptDisplay.toughnessColor] || "text-[#111]"}`}>
                {ptDisplay.toughness}
              </span>
            </div>
          </div>
        </div>
      )}

      {/* Floating loyalty (Mirrored inset logic) */}
      {obj.loyalty != null && (
        <div className="absolute -bottom-[3px] -right-[3px] z-20">
          <div className="rounded-full bg-gradient-to-b from-[#e2e4e6] to-[#888c91] p-[2px] shadow-[inset_0_1px_1px_rgba(255,255,255,0.9),inset_0_-1px_1px_rgba(0,0,0,0.5),0_2px_4px_rgba(0,0,0,0.8)] border border-black/80">
            {/* Dark inset inner pill */}
            <div className="bg-gray-800 border-[1px] border-amber-600/50 rounded-full px-2.5 py-[1px] min-w-[2.75rem] flex justify-center items-center shadow-[inset_0_2px_4px_rgba(0,0,0,0.8),inset_0_1px_2px_rgba(0,0,0,0.9),0_1px_0_rgba(255,255,255,0.2)]">
              <span className="font-bold text-[14px] text-amber-400 leading-none">
                {obj.loyalty}
              </span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}