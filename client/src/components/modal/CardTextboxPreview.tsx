import { useEffect, useState } from "react";
import { fetchCardImageUrl } from "../../services/scryfall.ts";

// some really rough guesses on these dims
const TEXTBOX_TOP = 0.600;
const TEXTBOX_BOTTOM = 0.90;
const CARD_ASPECT_H_OVER_W = 680 / 488;

interface Props {
  cardName: string;
  faceIndex?: number;
}

/**
 * Peeks at the rules-text band of a card's Scryfall image as a visual reminder
 * of the exact Oracle text. Clips to the text-box region using vertical
 * translation. Soft gradient applied.
 */
export function CardTextboxPreview({ cardName, faceIndex = 0 }: Props) {
  const [url, setUrl] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setUrl(null);
    fetchCardImageUrl(cardName, faceIndex, "normal")
      .then((u) => {
        if (!cancelled) setUrl(u);
      })
      .catch(() => {
        /* silent — prototype; no image ⇒ render nothing */
      });
    return () => {
      cancelled = true;
    };
  }, [cardName, faceIndex]);

  if (!url) return null;

  const bandHeight = TEXTBOX_BOTTOM - TEXTBOX_TOP;
  // Container height, expressed as padding-top % of container width, so the
  // box is exactly the height of the text-box band at whatever width we end up.
  const paddingTopPct = bandHeight * CARD_ASPECT_H_OVER_W * 100;

  return (
    <div
      className="relative w-full overflow-hidden rounded-[10px] border border-white/10 bg-black/40 shadow-inner"
      style={{ paddingTop: `${paddingTopPct}%` }}
    >
      <img
        src={url}
        alt=""
        draggable={false}
        className="absolute left-0 top-0 w-full"
        style={{ transform: `translateY(-${TEXTBOX_TOP * 100}%)` }}
      />
      <div className="pointer-events-none absolute inset-x-0 top-0 h-4 bg-gradient-to-b from-[#0b1020] via-[#0b1020]/70 to-transparent" />
      <div className="pointer-events-none absolute inset-x-0 bottom-0 h-4 bg-gradient-to-t from-[#0b1020] via-[#0b1020]/70 to-transparent" />
    </div>
  );
}
