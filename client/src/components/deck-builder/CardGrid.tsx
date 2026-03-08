import { motion, AnimatePresence } from "framer-motion";
import { type ScryfallCard, getCardImageSmall } from "../../services/scryfall";

interface CardGridProps {
  cards: ScryfallCard[];
  onAddCard: (card: ScryfallCard) => void;
}

function isStandardLegal(card: ScryfallCard): boolean {
  return card.legalities?.standard === "legal";
}

export function CardGrid({ cards, onAddCard }: CardGridProps) {
  return (
    <div className="grid auto-rows-min grid-cols-[repeat(auto-fill,minmax(130px,1fr))] gap-2 overflow-y-auto p-2">
      <AnimatePresence mode="popLayout">
        {cards.map((card) => {
          const imageUrl = getCardImageSmall(card);
          const legal = isStandardLegal(card);

          return (
            <motion.button
              key={card.id}
              layout
              initial={{ opacity: 0, scale: 0.9 }}
              animate={{ opacity: 1, scale: 1 }}
              exit={{ opacity: 0, scale: 0.9 }}
              transition={{ duration: 0.15 }}
              onClick={() => legal && onAddCard(card)}
              disabled={!legal}
              title={legal ? `Add ${card.name}` : `${card.name} - Not Standard legal`}
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
                  className="aspect-[488/680] w-full rounded-lg object-cover"
                  loading="lazy"
                />
              ) : (
                <div className="flex aspect-[488/680] w-full items-center justify-center rounded-lg bg-gray-800 text-xs text-gray-400">
                  {card.name}
                </div>
              )}

              {!legal && (
                <div className="absolute inset-0 flex items-center justify-center bg-black/50">
                  <span className="rounded bg-red-700 px-2 py-0.5 text-[10px] font-bold text-white">
                    Not Standard
                  </span>
                </div>
              )}

              {/* Hover tooltip */}
              <div className="pointer-events-none absolute bottom-0 left-0 right-0 translate-y-full bg-black/80 px-1.5 py-1 text-[10px] text-white transition-transform group-hover:translate-y-0">
                {card.name}
              </div>
            </motion.button>
          );
        })}
      </AnimatePresence>
    </div>
  );
}
