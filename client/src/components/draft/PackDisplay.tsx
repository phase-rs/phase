import { useEffect, useState } from "react";

import { useCardImage } from "../../hooks/useCardImage";
import { useDraftStore } from "../../stores/draftStore";
import type { DraftCardInstance, DraftPlayerView } from "../../adapter/draft-adapter";
import type { CardHoverInfo } from "../card/CardPreview";

// ── Card tile ───────────────────────────────────────────────────────────

interface PackCardProps {
  card: DraftCardInstance;
  isSelected: boolean;
  onSelect: (instanceId: string) => void;
  onConfirm: () => void;
  onHover: (info: CardHoverInfo | null) => void;
}

function PackCard({
  card,
  isSelected,
  onSelect,
  onConfirm,
  onHover,
}: PackCardProps) {
  const { src, isLoading } = useCardImage(card.name, {
    size: "normal",
    sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number },
  });

  return (
    <div
      className={`relative cursor-pointer overflow-hidden rounded-[14px] transition-all duration-150 ${
        isSelected
          ? "z-10 scale-105 ring-2 ring-amber-400 shadow-lg shadow-amber-400/20"
          : "ring-1 ring-white/10 hover:scale-[1.02] hover:ring-white/20"
      }`}
      onMouseEnter={() => onHover({ name: card.name, sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number } })}
      onMouseLeave={() => onHover(null)}
    >
      <button
        onClick={() => onSelect(card.instance_id)}
        className="w-full"
      >
        {isLoading || !src ? (
          <div className="flex aspect-[488/680] animate-pulse items-center justify-center bg-white/5">
            <span className="px-2 text-center text-xs text-white/40">{card.name}</span>
          </div>
        ) : (
          <img
            src={src}
            alt={card.name}
            draggable={false}
            className="aspect-[488/680] w-full object-cover"
          />
        )}
      </button>
      <div className="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/80 to-transparent px-3 py-2">
        {isSelected ? (
          <button
            onClick={onConfirm}
            className="w-full rounded-lg bg-amber-500 py-0.5 text-xs font-semibold text-black transition-colors hover:bg-amber-400"
          >
            Confirm Pick
          </button>
        ) : (
          <span className="line-clamp-1 text-[10px] leading-tight text-white/80">
            {card.name}
          </span>
        )}
      </div>
    </div>
  );
}

// ── Rarity helpers ─────────────────────────────────────────────────────

const RARITY_ORDER = ["mythic", "rare", "uncommon", "common"] as const;

const RARITY_LABELS: Record<string, string> = {
  mythic: "Mythic Rare",
  rare: "Rare",
  uncommon: "Uncommon",
  common: "Common",
};

// Distinct hues per rarity (mythic burnt-orange vs rare pale-gold reads at a glance),
// with a matching left-rule accent on the section header.
const RARITY_STYLES: Record<string, { text: string; accent: string }> = {
  mythic: { text: "text-orange-400", accent: "border-orange-400/55" },
  rare: { text: "text-amber-200", accent: "border-amber-300/45" },
  uncommon: { text: "text-slate-300", accent: "border-slate-300/30" },
  common: { text: "text-white/45", accent: "border-white/15" },
};

const RARITY_STYLE_FALLBACK = { text: "text-white/45", accent: "border-white/15" } as const;

function groupByRarity(cards: DraftCardInstance[]) {
  const groups: [string, DraftCardInstance[]][] = [];
  for (const rarity of RARITY_ORDER) {
    const matched = cards.filter((c) => c.rarity === rarity);
    if (matched.length > 0) groups.push([rarity, matched]);
  }
  const unmatched = cards.filter(
    (c) => !RARITY_ORDER.includes(c.rarity as (typeof RARITY_ORDER)[number]),
  );
  if (unmatched.length > 0) groups.push(["other", unmatched]);
  return groups;
}

// ── Main component ──────────────────────────────────────────────────────

interface PackDisplayProps {
  onCardHover: (info: CardHoverInfo | null) => void;
  /** Show the "Auto-pick" button when the active draft mode supports it. */
  showAutoPick?: boolean;
  view?: DraftPlayerView | null;
  selectedCard?: string | null;
  onSelectCard?: (instanceId: string | null) => void;
  onConfirmPick?: () => Promise<void> | void;
  onAutoPick?: () => Promise<void> | void;
}

export function PackDisplay({
  onCardHover,
  showAutoPick = false,
  view: viewOverride,
  selectedCard: selectedCardOverride,
  onSelectCard,
  onConfirmPick,
  onAutoPick,
}: PackDisplayProps) {
  const quickView = useDraftStore((s) => s.view);
  const quickSelectedCard = useDraftStore((s) => s.selectedCard);
  const quickSelectCard = useDraftStore((s) => s.selectCard);
  const quickConfirmPick = useDraftStore((s) => s.confirmPick);
  const quickAutoPickCard = useDraftStore((s) => s.autoPickCard);
  const [autoPicking, setAutoPicking] = useState(false);

  const view = viewOverride !== undefined ? viewOverride : quickView;
  const selectedCard = selectedCardOverride !== undefined
    ? selectedCardOverride
    : quickSelectedCard;
  const selectCard = onSelectCard ?? quickSelectCard;
  const confirmPick = onConfirmPick ?? quickConfirmPick;
  const autoPickCard = onAutoPick ?? quickAutoPickCard;

  useEffect(() => {
    if (view?.current_pack?.length === 1 && !selectedCard) {
      selectCard(view.current_pack[0].instance_id);
    }
  }, [view?.current_pack, selectedCard, selectCard]);

  if (!view) return null;

  const pack = view.current_pack;

  if (!pack || pack.length === 0) {
    return (
      <div className="flex items-center justify-center py-12 text-white/40">
        Waiting for next pack...
      </div>
    );
  }

  const handleAutoPick = async () => {
    setAutoPicking(true);
    try {
      await autoPickCard();
    } finally {
      setAutoPicking(false);
    }
  };

  const sections = groupByRarity(pack);

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <span className="text-xs text-white/40">{pack.length} cards in pack</span>
        {showAutoPick && (
          <button
            type="button"
            onClick={handleAutoPick}
            disabled={autoPicking}
            className="rounded-lg border border-white/15 bg-white/[0.04] px-3 py-1 text-xs font-medium text-white/80 transition-colors hover:border-white/25 hover:bg-white/[0.08] disabled:cursor-not-allowed disabled:opacity-50"
          >
            {autoPicking ? "Picking…" : "Auto-pick"}
          </button>
        )}
      </div>
      {sections.map(([rarity, cards]) => {
        const rarityStyle = RARITY_STYLES[rarity] ?? RARITY_STYLE_FALLBACK;
        return (
          <div key={rarity}>
            <h3
              className={`mb-2 border-l-2 pl-2 text-xs font-semibold uppercase tracking-wider ${rarityStyle.text} ${rarityStyle.accent}`}
            >
              {RARITY_LABELS[rarity] ?? rarity}
            </h3>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
              {cards.map((card) => (
                <PackCard
                  key={card.instance_id}
                  card={card}
                  isSelected={selectedCard === card.instance_id}
                  onSelect={selectCard}
                  onConfirm={confirmPick}
                  onHover={onCardHover}
                />
              ))}
            </div>
          </div>
        );
      })}
    </div>
  );
}
