import { motion, AnimatePresence } from "framer-motion";
import { scryfallLegalityKey, type ScryfallCard } from "../../services/scryfall";
import { useLongPress } from "../../hooks/useLongPress";
import type { BrowserLegalityFilter } from "./CardSearch";
import { LegalityBadge } from "./LegalityBadge";
import { mouseHoverPreview } from "./hoverPreview";

interface CardGridProps {
  cards: ScryfallCard[];
  onAddCard: (card: ScryfallCard) => void;
  onCardHover?: (cardName: string | null) => void;
  cardCounts?: Map<string, number>;
  legalityFormat?: BrowserLegalityFilter;
}

function getArtCropUrl(card: ScryfallCard): string {
  return (
    card.image_uris?.art_crop ??
    card.card_faces?.[0]?.image_uris?.art_crop ??
    ""
  );
}

function isFormatLegal(card: ScryfallCard, format: BrowserLegalityFilter): boolean {
  if (format === "all") return true;
  const legalityKey = scryfallLegalityKey(format);
  if (!legalityKey) return true;
  return card.legalities?.[legalityKey] === "legal";
}

export function CardGrid({
  cards,
  onAddCard,
  onCardHover,
  cardCounts,
  legalityFormat = "all",
}: CardGridProps) {
  return (
    <div className="grid auto-rows-min grid-cols-[repeat(auto-fill,minmax(110px,1fr))] gap-2 overflow-y-auto p-2 sm:grid-cols-[repeat(auto-fill,minmax(130px,1fr))]">
      <AnimatePresence mode="popLayout">
        {cards.map((card) => (
          <CardGridTile
            key={card.id ?? card.name}
            card={card}
            legal={isFormatLegal(card, legalityFormat)}
            count={cardCounts?.get(card.name)}
            legalityFormat={legalityFormat}
            onAddCard={onAddCard}
            onCardHover={onCardHover}
          />
        ))}
      </AnimatePresence>
    </div>
  );
}

interface CardGridTileProps {
  card: ScryfallCard;
  legal: boolean;
  count: number | undefined;
  legalityFormat: BrowserLegalityFilter;
  onAddCard: (card: ScryfallCard) => void;
  onCardHover?: (cardName: string | null) => void;
}

function CardGridTile({
  card,
  legal,
  count,
  legalityFormat,
  onAddCard,
  onCardHover,
}: CardGridTileProps) {
  const imageUrl = getArtCropUrl(card);
  const formatLabel = legalityFormat === "all"
    ? "All"
    : legalityFormat.charAt(0).toUpperCase() + legalityFormat.slice(1);

  // Touch model (mirrors MobileHandDrawer's DrawerCard): tap adds the card,
  // long-press opens the preview. firedRef suppresses the click that follows a
  // long-press so a long-press never also adds the card. Desktop is unaffected
  // (handlers are touch-only; hover still drives the preview).
  const { handlers, firedRef } = useLongPress(() => onCardHover?.(card.name));

  const handleClick = () => {
    if (firedRef.current) {
      firedRef.current = false;
      return;
    }
    if (legal) onAddCard(card);
  };

  return (
    <motion.button
      type="button"
      layout
      initial={{ opacity: 0, scale: 0.9 }}
      animate={{ opacity: 1, scale: 1 }}
      exit={{ opacity: 0, scale: 0.9 }}
      transition={{ duration: 0.15 }}
      onClick={handleClick}
      {...mouseHoverPreview(onCardHover, card.name)}
      {...handlers}
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
      {legalityFormat !== "all" && (
        <div className="absolute left-1 top-1">
          {card.legalities && <LegalityBadge legalities={card.legalities} format={legalityFormat} />}
        </div>
      )}

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
}
