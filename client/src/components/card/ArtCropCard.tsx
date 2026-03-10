import { useCardImage } from "../../hooks/useCardImage.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { computePTDisplay } from "../../viewmodel/cardProps.ts";
import { PTBox } from "../board/PTBox.tsx";
import { getBevelBorderStyle } from "./cardFrame.ts";

interface ArtCropCardProps {
  objectId: number;
}

const COUNTER_COLORS: Record<string, string> = {
  Plus1Plus1: "bg-green-600",
  Minus1Minus1: "bg-red-600",
  Loyalty: "bg-amber-600",
};

export function ArtCropCard({ objectId }: ArtCropCardProps) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  const inspectObject = useUiStore((s) => s.inspectObject);

  const cardName = obj?.name ?? "";
  const { src, isLoading } = useCardImage(cardName, { size: "art_crop" });

  if (!obj) return null;

  const hasDfc = obj.back_face != null;
  const isToken = obj.card_id === 0;
  const borderWidth = isToken ? 2 : 3;
  const bevelBorder = getBevelBorderStyle(obj.color, borderWidth);
  const ptDisplay = computePTDisplay(obj);
  const counters = Object.entries(obj.counters);

  if (isLoading || !src) {
    return (
      <div className="flex flex-col">
        <div
          className="truncate rounded-t-sm bg-black/70 px-1.5 py-0.5 text-left text-[9px] font-bold text-gray-300"
          style={{ width: "var(--art-crop-w)" }}
        >
          {cardName}
        </div>
        <div
          className="rounded-b-md bg-gray-700 animate-pulse"
          style={{
            width: "var(--art-crop-w)",
            height: "var(--art-crop-h)",
            ...bevelBorder,
          }}
          aria-label={`Loading ${cardName}`}
        />
      </div>
    );
  }

  return (
    <div className="flex flex-col">
      {/* Card name — left-aligned title above art with background */}
      <div
        className="truncate rounded-t-sm bg-black/70 px-1.5 py-0.5 text-left text-[9px] font-bold text-gray-300"
        style={{ width: "var(--art-crop-w)" }}
      >
        {cardName}
      </div>

      <div
        className="relative rounded-b-md overflow-hidden"
        style={{
          width: "var(--art-crop-w)",
          height: "var(--art-crop-h)",
          ...bevelBorder,
        }}
      >
        {/* Art crop image — unobscured */}
        <img
          src={src}
          alt={cardName}
          className="w-full h-full object-cover"
          draggable={false}
        />

        {/* P/T box overlay */}
        {ptDisplay && <PTBox ptDisplay={ptDisplay} />}

        {/* Loyalty shield */}
        {obj.loyalty != null && (
          <div className="absolute bottom-0 left-1/2 -translate-x-1/2 z-20 bg-gray-900/90 border border-amber-400 rounded-full px-2 py-0.5 text-xs font-bold text-amber-300">
            {obj.loyalty}
          </div>
        )}

        {/* Counter badges */}
        {counters.length > 0 && (
          <div className="absolute top-0.5 right-0.5 z-20 flex flex-col gap-0.5">
            {counters.map(([type, count]) => (
              <div
                key={type}
                className={`rounded-full w-5 h-5 flex items-center justify-center text-[9px] font-bold text-white ${COUNTER_COLORS[type] ?? "bg-purple-600"}`}
              >
                {count}
              </div>
            ))}
          </div>
        )}

        {/* Unimplemented mechanics badge */}
        {obj.has_unimplemented_mechanics && (
          <span
            className="absolute top-0.5 left-0.5 bg-amber-500 text-black text-[8px] font-bold rounded-sm px-0.5 leading-tight"
            title="This card has mechanics not yet fully implemented"
          >
            !
          </span>
        )}

        {/* DFC indicator — hover to peek at back face */}
        {hasDfc && (
          <button
            type="button"
            className="absolute bottom-0.5 left-0.5 z-20 bg-gray-900/80 border border-gray-500 rounded-sm px-1 py-0.5 text-[8px] font-bold text-gray-300 hover:bg-gray-700/90 hover:text-white cursor-pointer"
            title={`Back face: ${obj.back_face!.name}`}
            onMouseEnter={() => inspectObject(objectId, 1)}
            onMouseLeave={() => inspectObject(objectId, 0)}
          >
            DFC
          </button>
        )}
      </div>
    </div>
  );
}
