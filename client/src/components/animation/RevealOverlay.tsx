import { AnimatePresence, motion } from "framer-motion";
import { useMemo } from "react";

import { useCardImage } from "../../hooks/useCardImage.ts";
import { useGameStore } from "../../stores/gameStore.ts";

function RevealCard({ cardName }: { cardName: string }) {
  const { src } = useCardImage(cardName, { size: "small" });

  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: -14, scale: 0.85 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0, scale: 0.85 }}
      transition={{ type: "spring", stiffness: 320, damping: 26 }}
      className="overflow-hidden rounded-lg shadow-xl ring-2 ring-amber-400/80"
      style={{ width: 72, height: 100 }}
    >
      {src ? (
        <img
          src={src}
          alt={cardName}
          className="h-full w-full object-cover"
          draggable={false}
        />
      ) : (
        <div className="h-full w-full border border-gray-600 bg-gray-700" />
      )}
    </motion.div>
  );
}

/**
 * Passive overlay surfacing a multi-card top-of-library reveal to every player.
 *
 * CR 701.20b: revealed cards are public to all players. When an effect reveals
 * several top-of-library cards at once ("reveal the top N", or "look at the top
 * N, reveal any number" — e.g. Lead the Stampede), the engine publishes them to
 * `revealed_cards` for the duration of the choice and un-redacts them in every
 * viewer's filtered state. `LibraryPile` only renders the single top card, so
 * the cards past the first were otherwise invisible (issues #2366, #2005).
 *
 * Scope is deliberately the gap `LibraryPile` leaves: 2+ revealed cards that are
 * still in a library zone. A single persistently-revealed top card (Future
 * Sight, Oracle of Mul Daya) is already shown by `LibraryPile`; hand reveals
 * (Thoughtseize) are not top-of-library and are excluded by the library filter.
 * The overlay mounts below modals (z-30), so the choosing player's DigChoice
 * modal occludes it while watching players — who have no modal — see the reveal.
 */
export function RevealOverlay() {
  const gameState = useGameStore((s) => s.gameState);

  const revealed = useMemo(() => {
    const ids = gameState?.revealed_cards;
    if (!gameState || !ids || ids.length < 2) return [];

    const libraryIds = new Set<number>();
    for (const player of gameState.players) {
      for (const id of player.library ?? []) libraryIds.add(id);
    }

    const cards: { id: number; name: string }[] = [];
    for (const id of ids) {
      if (!libraryIds.has(id)) continue;
      const obj = gameState.objects[id];
      // `revealed_cards` is the public set, so the engine un-redacts these for
      // every viewer; guard against a redacted name defensively only.
      if (obj?.name && obj.name !== "Hidden Card") {
        cards.push({ id, name: obj.name });
      }
    }
    return cards;
  }, [gameState]);

  if (revealed.length < 2) return null;

  return (
    <div className="pointer-events-none fixed inset-x-0 top-20 z-30 flex justify-center px-4">
      <motion.div
        layout
        initial={{ opacity: 0, y: -8 }}
        animate={{ opacity: 1, y: 0 }}
        exit={{ opacity: 0 }}
        className="flex max-w-full items-center gap-2 overflow-x-auto rounded-2xl bg-black/70 px-4 py-3 shadow-2xl ring-1 ring-amber-400/40 backdrop-blur-sm"
      >
        {/* Eye icon — signals "revealed" without a locale string. */}
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth={2}
          strokeLinecap="round"
          strokeLinejoin="round"
          className="h-5 w-5 shrink-0 text-amber-300"
          aria-hidden="true"
        >
          <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7Z" />
          <circle cx="12" cy="12" r="3" />
        </svg>
        <AnimatePresence mode="popLayout">
          {revealed.map((card) => (
            <RevealCard key={card.id} cardName={card.name} />
          ))}
        </AnimatePresence>
      </motion.div>
    </div>
  );
}
