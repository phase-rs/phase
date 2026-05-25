import { useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

const EMOTES = ["Good game", "Nice play", "Thinking...", "Hello!", "Oops"] as const;
const EMOTE_KEYS = ["goodGame", "nicePlay", "thinking", "hello", "oops"] as const;
const EMOTE_DISPLAY_MS = 3000;

/**
 * Emotes travel the wire as their canonical English text (`onSendEmote(emote)`),
 * so a received emote arrives in English regardless of the sender's locale.
 * Reverse-look-up the fixed emote set to render it in the *viewer's* language;
 * an unrecognized string (shouldn't occur) falls back to the raw text.
 */
function localizedEmote(text: string, t: TFunction<"multiplayer">): string {
  const idx = (EMOTES as readonly string[]).indexOf(text);
  return idx >= 0 ? t(`emoteOverlay.options.${EMOTE_KEYS[idx]}`) : text;
}

interface EmoteOverlayProps {
  onSendEmote: (emote: string) => void;
  receivedEmote: string | null;
}

export function EmoteOverlay({ onSendEmote, receivedEmote }: EmoteOverlayProps) {
  const { t } = useTranslation("multiplayer");
  const [showPanel, setShowPanel] = useState(false);
  const [displayedEmote, setDisplayedEmote] = useState<{ text: string; id: number } | null>(null);
  const nextId = useRef(0);

  useEffect(() => {
    if (!receivedEmote) return;
    const id = nextId.current++;
    setDisplayedEmote({ text: receivedEmote, id });
    const timer = setTimeout(() => {
      setDisplayedEmote((prev) => (prev?.id === id ? null : prev));
    }, EMOTE_DISPLAY_MS);
    return () => clearTimeout(timer);
  }, [receivedEmote]);

  return (
    <>
      {/* Received emote display — near top-center (opponent area) */}
      <div
        className="fixed left-1/2 z-40 -translate-x-1/2"
        style={{
          top: "calc(env(safe-area-inset-top) + var(--game-top-overlay-offset, 0px) + 6rem)",
        }}
      >
        <AnimatePresence>
          {displayedEmote && (
            <motion.div
              key={displayedEmote.id}
              className="rounded-full bg-black/70 px-4 py-2 text-sm font-medium text-white backdrop-blur-sm"
              initial={{ opacity: 0, y: -10, scale: 0.9 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: -10, scale: 0.9 }}
              transition={{ duration: 0.25 }}
            >
              {localizedEmote(displayedEmote.text, t)}
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      {/* Emote button bar — bottom-left, above player zones */}
      <div
        className="fixed z-30"
        style={{
          bottom: "calc(env(safe-area-inset-bottom) + 7rem)",
          left: "calc(env(safe-area-inset-left) + 1rem)",
        }}
      >
        <button
          onClick={() => setShowPanel((v) => !v)}
          className="flex h-8 w-8 items-center justify-center rounded-full bg-gray-800/80 text-gray-400 transition-colors hover:bg-gray-700/80 hover:text-gray-200"
          aria-label={t("emoteOverlay.ariaLabel")}
        >
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
            <path fillRule="evenodd" d="M10 18a8 8 0 1 0 0-16 8 8 0 0 0 0 16Zm3.536-4.464a.75.75 0 1 0-1.061-1.061 3.5 3.5 0 0 1-4.95 0 .75.75 0 0 0-1.06 1.06 5 5 0 0 0 7.07 0ZM9 8.5c0 .828-.448 1.5-1 1.5s-1-.672-1-1.5S7.448 7 8 7s1 .672 1 1.5Zm3 1.5c.552 0 1-.672 1-1.5S12.552 7 12 7s-1 .672-1 1.5.448 1.5 1 1.5Z" clipRule="evenodd" />
          </svg>
        </button>

        <AnimatePresence>
          {showPanel && (
            <motion.div
              className="absolute bottom-full left-0 mb-2 flex flex-col gap-1"
              initial={{ opacity: 0, y: 10 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: 10 }}
              transition={{ duration: 0.15 }}
            >
              {EMOTES.map((emote, i) => (
                <button
                  key={emote}
                  onClick={() => {
                    onSendEmote(emote);
                    setShowPanel(false);
                  }}
                  className="whitespace-nowrap rounded-lg bg-gray-800/90 px-3 py-1.5 text-left text-xs font-medium text-gray-200 transition-colors hover:bg-gray-700 hover:text-white"
                >
                  {t(`emoteOverlay.options.${EMOTE_KEYS[i]}`)}
                </button>
              ))}
            </motion.div>
          )}
        </AnimatePresence>
      </div>
    </>
  );
}
