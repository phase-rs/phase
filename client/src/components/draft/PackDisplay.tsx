import { useCardImage } from "../../hooks/useCardImage";
import { useDraftStore } from "../../stores/draftStore";
import type { DraftCardInstance } from "../../adapter/draft-adapter";

// ── Card tile ───────────────────────────────────────────────────────────

interface PackCardProps {
  card: DraftCardInstance;
  isSelected: boolean;
  onSelect: (instanceId: string) => void;
}

function PackCard({ card, isSelected, onSelect }: PackCardProps) {
  const { src, isLoading } = useCardImage(card.name, { size: "normal" });

  return (
    <button
      onClick={() => onSelect(card.instance_id)}
      className={`relative rounded-lg overflow-hidden transition-all duration-150 cursor-pointer ${
        isSelected
          ? "ring-2 ring-amber-400 scale-105 z-10 shadow-lg shadow-amber-400/20"
          : "ring-1 ring-gray-700 hover:ring-gray-500 hover:scale-[1.02]"
      }`}
    >
      {isLoading || !src ? (
        <div className="aspect-[488/680] bg-gray-700 animate-pulse flex items-center justify-center">
          <span className="text-xs text-gray-400 px-2 text-center">{card.name}</span>
        </div>
      ) : (
        <img
          src={src}
          alt={card.name}
          draggable={false}
          className="w-full aspect-[488/680] object-cover"
        />
      )}
      <div className="absolute bottom-0 inset-x-0 bg-gradient-to-t from-black/80 to-transparent px-1.5 py-1">
        <span className="text-[10px] text-gray-200 leading-tight line-clamp-1">
          {card.name}
        </span>
      </div>
    </button>
  );
}

// ── Main component ──────────────────────────────────────────────────────

/** Card image grid for pack picks. Per D-05: click to select, confirm to pick. */
export function PackDisplay() {
  const view = useDraftStore((s) => s.view);
  const selectedCard = useDraftStore((s) => s.selectedCard);
  const selectCard = useDraftStore((s) => s.selectCard);
  const confirmPick = useDraftStore((s) => s.confirmPick);

  if (!view) return null;

  const pack = view.current_pack;

  if (!pack || pack.length === 0) {
    return (
      <div className="flex items-center justify-center py-12 text-gray-400">
        Waiting for next pack...
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-3">
        {pack.map((card) => (
          <PackCard
            key={card.instance_id}
            card={card}
            isSelected={selectedCard === card.instance_id}
            onSelect={selectCard}
          />
        ))}
      </div>

      <div className="flex justify-center">
        <button
          onClick={confirmPick}
          disabled={!selectedCard}
          className={`px-6 py-2 rounded-lg font-medium text-sm transition-colors ${
            selectedCard
              ? "bg-amber-500 hover:bg-amber-400 text-black cursor-pointer"
              : "bg-gray-700 text-gray-500 cursor-not-allowed"
          }`}
        >
          Confirm Pick
        </button>
      </div>
    </div>
  );
}
