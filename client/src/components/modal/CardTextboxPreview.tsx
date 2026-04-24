import { useCardImage } from "../../hooks/useCardImage.ts";

// Rough fractions of card height where the text box sits on a standard frame.
const TOP = 0.60;
const BOTTOM = 0.90;
// Scryfall is 488×680; aspect-ratio keeps the container sized to
// exactly the (BOTTOM - TOP) band of the card.
const CARD_W = 488;
const CARD_H = 680;

/**
 * Peeks at the rules-text band of a card's Scryfall image as a reminder of the
 * exact Oracle text. Clips to the text-box region by translating the full
 * image up inside an aspect-locked container, with gradient fades at the edges.
 */
export function CardTextboxPreview({ cardName }: { cardName: string }) {
  const { src } = useCardImage(cardName, { size: "normal" });

  if (!src) return null;

  return (
    <div
      className="relative w-full overflow-hidden rounded-[10px] border border-white/10 bg-black/40 shadow-inner"
      style={{ aspectRatio: `${CARD_W} / ${CARD_H * (BOTTOM - TOP)}` }}
    >
      <img
        src={src}
        alt=""
        draggable={false}
        className="absolute inset-x-0 top-0 w-full"
        style={{ transform: `translateY(-${TOP * 100}%)` }}
      />
      <div className="pointer-events-none absolute inset-x-0 top-0 h-4 bg-gradient-to-b from-[#0b1020] via-[#0b1020]/70 to-transparent" />
      <div className="pointer-events-none absolute inset-x-0 bottom-0 h-4 bg-gradient-to-t from-[#0b1020] via-[#0b1020]/70 to-transparent" />
    </div>
  );
}
