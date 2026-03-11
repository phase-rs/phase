import { motion, AnimatePresence } from "framer-motion";
import type { ScryfallCard } from "../../services/scryfall";
import { LegalityBadge } from "./LegalityBadge";

interface CardGridProps {
  cards: ScryfallCard[];
  onAddCard: (card: ScryfallCard) => void;
  onCardHover?: (cardName: string | null) => void;
  cardCounts?: Map<string, number>;
  format?: string;
}

function getArtCropUrl(card: ScryfallCard): string {
  return (
    card.image_uris?.art_crop ??
    card.card_faces?.[0]?.image_uris?.art_crop ??
    ""
  );
}

function isFormatLegal(card: ScryfallCard, format: string): boolean {
  return card.legalities?.[format] === "legal";
}

export function CardGrid({
  cards,
  onAddCard,
  onCardHover,
  cardCounts,
  format = "standard",
}: CardGridProps) {
  return (
    <div className="grid auto-rows-min grid-cols-[repeat(auto-fill,minmax(130px,1fr))] gap-2 overflow-y-auto p-2">
      <AnimatePresence mode="popLayout">
        {cards.map((card) => {
          const imageUrl = getArtCropUrl(card);
          const legal = isFormatLegal(card, format);
          const count = cardCounts?.get(card.name);
          const formatLabel = format.charAt(0).toUpperCase() + format.slice(1);

          return (
            <motion.button
              key={card.id}
              layout
              initial={{ opacity: 0, scale: 0.9 }}
              animate={{ opacity: 1, scale: 1 }}
              exit={{ opacity: 0, scale: 0.9 }}
              transition={{ duration: 0.15 }}
              onClick={() => legal && onAddCard(card)}
              onMouseEnter={() => onCardHover?.(card.name)}
              onMouseLeave={() => onCardHover?.(null)}
              disabled={!legal}
              title={legal ? `Add ${card.name}` : `${card.name} - Not ${formatLabel} legal`}
              className={`group relative cursor-pointer overflow-hidden rounded-lg transition-transform hover:scale-105 ${
                legal
                  ? "ring-2 ring-transparent hover:ring-green-500"
                  : "cursor-not-allowed opacity-60 ring-2 ring-red-600"
              }`}
            >
              {imageUrl ? (
                <img
                  src={imageUrl}
                  alt={card.name}
                  className="aspect-[4/3] w-full rounded-lg object-cover"
                  loading="lazy"
                />
              ) : (
                <div className="flex aspect-[4/3] w-full items-center justify-center rounded-lg bg-gray-800 text-xs text-gray-400">
                  {card.name}
                </div>
              )}

              {!legal && (
                <div className="absolute inset-0 flex items-center justify-center bg-black/50">
                  <span className="rounded bg-red-700 px-2 py-0.5 text-[10px] font-bold text-white">
                    Not {formatLabel}
                  </span>
                </div>
              )}

              {/* Legality badge */}
              <div className="absolute left-1 top-1">
                <LegalityBadge legalities={card.legalities} format={format} />
              </div>

              {/* Card count badge */}
              {count !== undefined && count > 0 && (
                <div className="absolute right-1 top-1 flex h-5 w-5 items-center justify-center rounded-full bg-blue-600 text-[10px] font-bold text-white shadow">
                  {count}
                </div>
              )}

              {/* Card name - always visible */}
              <div className="pointer-events-none absolute bottom-0 left-0 right-0 bg-black/70 px-1.5 py-0.5 text-[10px] text-white truncate">
                {card.name}
              </div>
            </motion.button>
          );
        })}
      </AnimatePresence>
    </div>
  );
}
