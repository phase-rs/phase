import { useEffect, useState } from "react";

import type { CardType } from "../../adapter/types.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";

// Scryfall is 488×680; aspect-ratio keeps the container sized to
// exactly the (BOTTOM - TOP) band of the card.
const CARD_W = 488;
const CARD_H = 680;

interface Band {
  top: number;
  bottom: number;
}

// Rough fractions of card height where each frame's rules-text band sits.
// Most relevant for cards whose abilities a player chooses between in a modal
// (sagas chapters, planeswalker loyalty abilities, class levels, etc.).
const STANDARD: Band = { top: 0.60, bottom: 0.90 };
const SAGA: Band = { top: 0.10, bottom: 0.90 };
const PLANESWALKER: Band = { top: 0.45, bottom: 0.93 };
const BATTLE: Band = { top: 0.50, bottom: 0.92 };
const CLASS: Band = { top: 0.45, bottom: 0.92 };
const CASE: Band = { top: 0.55, bottom: 0.92 };

function bandFor(cardTypes: CardType | undefined): Band {
  if (!cardTypes) return STANDARD;
  const { core_types, subtypes } = cardTypes;
  if (subtypes.includes("Saga")) return SAGA;
  if (subtypes.includes("Class")) return CLASS;
  if (subtypes.includes("Case")) return CASE;
  if (core_types.includes("Planeswalker")) return PLANESWALKER;
  if (core_types.includes("Battle")) return BATTLE;
  return STANDARD;
}

/**
 * Peeks at the rules-text band of a card's Scryfall image as a reminder of the
 * exact Oracle text. Clips to the text-box region by translating the full
 * image up inside an aspect-locked container, with gradient fades at the edges.
 *
 * The band is chosen from `cardTypes` so frames that don't follow the standard
 * creature/spell layout (sagas, planeswalkers, battles, classes, cases) crop
 * to their actual rules-text region.
 */
export function CardTextboxPreview({
  cardName,
  cardTypes,
}: {
  cardName: string;
  cardTypes?: CardType;
}) {
  const { src, isLoading } = useCardImage(cardName, { size: "normal" });
  const [artError, setArtError] = useState(false);
  useEffect(() => setArtError(false), [src]);

  // Still resolving — stay absent rather than flash a band into the modal.
  if (isLoading) return null;

  // Settled with no art (issue #6156), or the art 404'd. Returning null here
  // erased the card-identification band from the decision modals that host
  // this (ChoiceModal, PermanentTypeSlotModal, AlternativeCostModal) for
  // exactly the cards whose identity is hardest to infer. Name it instead.
  if (!src || artError) {
    return (
      <div
        className="flex w-full items-center justify-center overflow-hidden rounded-[10px] border border-white/10 bg-black/40 px-3 py-2 shadow-inner"
        role="img"
        aria-label={cardName}
      >
        <span className="truncate text-xs font-medium text-gray-300">{cardName}</span>
      </div>
    );
  }

  const { top, bottom } = bandFor(cardTypes);

  return (
    <div
      className="relative w-full overflow-hidden rounded-[10px] border border-white/10 bg-black/40 shadow-inner"
      style={{ aspectRatio: `${CARD_W} / ${CARD_H * (bottom - top)}` }}
    >
      <img
        src={src}
        alt=""
        draggable={false}
        onError={() => setArtError(true)}
        className="absolute inset-x-0 top-0 w-full"
        style={{ transform: `translateY(-${top * 100}%)` }}
      />
      <div className="pointer-events-none absolute inset-x-0 top-0 h-4 bg-gradient-to-b from-[#0b1020] via-[#0b1020]/70 to-transparent" />
      <div className="pointer-events-none absolute inset-x-0 bottom-0 h-4 bg-gradient-to-t from-[#0b1020] via-[#0b1020]/70 to-transparent" />
    </div>
  );
}
