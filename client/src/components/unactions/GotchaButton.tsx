import { AnimatePresence, motion } from "framer-motion";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useSpectatorMode } from "../../hooks/useSpectatorMode.ts";
import { useGameStore } from "../../stores/gameStore.ts";

/** How long the Gotcha! button stays up once the engine opens the window. */
export const GOTCHA_BUTTON_MS = 2000;

/**
 * The "Gotcha!" button. The engine opens a window (`un_actions.gotcha_offers`)
 * when an opponent's gesture satisfies a Gotcha ability the viewer holds; the
 * button then shows for two seconds — "you have to be quick" — and disappears.
 * The engine closes the window itself at the next game action, so a late click
 * is rejected there; the timer here only governs what is displayed.
 */
export function GotchaButton() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const spectator = useSpectatorMode();
  const offers = useGameStore((s) => s.gameState?.un_actions?.gotcha_offers);
  const objects = useGameStore((s) => s.gameState?.objects);
  // Each engine-issued offer id carries its own two-second fuse, so a new offer
  // never revives an expired one and claiming one never restarts the others.
  const [expiredIds, setExpiredIds] = useState<ReadonlySet<number>>(new Set());
  const timers = useRef(new Map<number, number>());

  useEffect(() => {
    const live = new Set((offers ?? []).map((offer) => offer.id));
    for (const [id, timer] of timers.current) {
      if (!live.has(id)) {
        window.clearTimeout(timer);
        timers.current.delete(id);
      }
    }
    // Forget expiries of closed windows so an identical later window shows again.
    setExpiredIds((prev) => {
      const kept = [...prev].filter((id) => live.has(id));
      return kept.length === prev.size ? prev : new Set(kept);
    });
    for (const id of live) {
      if (timers.current.has(id)) continue;
      timers.current.set(
        id,
        window.setTimeout(
          () => setExpiredIds((prev) => new Set(prev).add(id)),
          GOTCHA_BUTTON_MS,
        ),
      );
    }
  }, [offers]);

  useEffect(() => {
    const active = timers.current;
    return () => {
      for (const timer of active.values()) window.clearTimeout(timer);
      active.clear();
    };
  }, []);

  const visible = (offers ?? []).filter((offer) => !expiredIds.has(offer.id));
  if (spectator || visible.length === 0) return null;

  return (
    <div className="fixed inset-x-0 bottom-40 z-[54] flex flex-wrap justify-center gap-3">
      <AnimatePresence>
        {visible.map((offer) => (
          <motion.button
            key={offer.id}
            type="button"
            className="relative overflow-hidden rounded-2xl border-4 border-yellow-200 bg-rose-600 px-8 py-4 text-3xl font-black uppercase tracking-widest text-white shadow-[0_0_40px_rgba(244,63,94,0.8)]"
            initial={{ scale: 0, rotate: -20 }}
            animate={{ scale: [0, 1.3, 1], rotate: [-20, 6, 0] }}
            exit={{ scale: 0, opacity: 0 }}
            whileHover={{ scale: 1.08 }}
            whileTap={{ scale: 0.92 }}
            onClick={() =>
              void dispatch({
                type: "UnAction",
                data: {
                  action: {
                    type: "SayGotcha",
                    data: {
                      source_id: offer.source.reference.object_id,
                      returns: offer.returns.reference.object_id,
                    },
                  },
                },
              })
            }
          >
            {t("unActions.gotcha.button")}
            {visible.length > 1 && objects?.[offer.returns.reference.object_id] && (
              <span className="block text-sm font-semibold normal-case tracking-normal">
                {objects[offer.returns.reference.object_id].name}
              </span>
            )}
            {/* The two-second fuse, burning down. */}
            <motion.span
              aria-hidden
              className="absolute inset-x-0 bottom-0 h-1.5 origin-left bg-yellow-200"
              initial={{ scaleX: 1 }}
              animate={{ scaleX: 0 }}
              transition={{ duration: GOTCHA_BUTTON_MS / 1000, ease: "linear" }}
            />
          </motion.button>
        ))}
      </AnimatePresence>
    </div>
  );
}
