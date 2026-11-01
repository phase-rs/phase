import { AnimatePresence, motion } from "framer-motion";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { UnTell, UnTellKind } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useSpectatorMode } from "../../hooks/useSpectatorMode.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";

type SpeechKind = Extract<UnTellKind, { type: "SaysWord" | "SaysNumber" }>["type"];

const PHYSICAL_ICONS = {
  TouchFace: "\u{1F926}",
  TouchTable: "\u{1F590}\u{FE0F}",
  Laugh: "\u{1F923}",
  FlickCards: "\u{1F0CF}",
} as const;

/**
 * The Un-actions menu: gestures a player can perform for silver-border cards
 * (Gotcha — "if an opponent touches their face ..."). The engine decides which
 * gestures are worth offering (`un_actions.menu`) and what each one does; this
 * component only lists them and submits the chosen `PerformTell`.
 *
 * Renders nothing unless the player opted in and some card in the game
 * responds to a gesture, so the menu never appears outside those cards.
 */
export function UnActionsMenu() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const playerId = usePlayerId();
  const spectator = useSpectatorMode();
  const unActions = useGameStore((s) => s.gameState?.un_actions);
  const [open, setOpen] = useState(false);
  const [speaking, setSpeaking] = useState<SpeechKind | null>(null);
  const [text, setText] = useState("");

  const enabled = unActions?.enabled_players?.includes(playerId) ?? false;
  const menu = unActions?.menu ?? [];
  if (spectator || !enabled || menu.length === 0) return null;

  const perform = (tell: UnTell) => {
    void dispatch({ type: "UnAction", data: { action: { type: "PerformTell", data: { tell } } } });
    setOpen(false);
    setSpeaking(null);
    setText("");
  };

  const label = (kind: UnTellKind): string => {
    switch (kind.type) {
      case "Physical":
        return t(`unActions.menu.${kind.data}`);
      case "SaysWord":
        return t("unActions.menu.saysWord");
      case "SaysNumber":
        return t("unActions.menu.saysNumber");
    }
  };

  return (
    <div className="fixed bottom-28 left-3 z-40 flex flex-col items-start gap-2">
      <AnimatePresence>
        {open && (
          <motion.div
            className="w-56 rounded-xl border border-fuchsia-300/30 bg-slate-950/95 p-2 shadow-2xl backdrop-blur-md"
            initial={{ opacity: 0, y: 12, scale: 0.95 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 12, scale: 0.95 }}
            role="menu"
            aria-label={t("unActions.menu.title")}
          >
            <p className="px-2 pb-1 text-xs font-semibold uppercase tracking-[0.2em] text-fuchsia-200">
              {t("unActions.menu.title")}
            </p>
            {speaking ? (
              <form
                className="flex flex-col gap-2 p-1"
                onSubmit={(event) => {
                  event.preventDefault();
                  const spoken = text.trim();
                  if (spoken) perform({ type: "Said", data: { text: spoken } });
                }}
              >
                <input
                  autoFocus
                  value={text}
                  onChange={(event) => setText(event.target.value)}
                  inputMode={speaking === "SaysNumber" ? "numeric" : "text"}
                  maxLength={64}
                  placeholder={t(
                    speaking === "SaysNumber"
                      ? "unActions.menu.numberPlaceholder"
                      : "unActions.menu.wordPlaceholder",
                  )}
                  className="rounded-md border border-white/15 bg-slate-900 px-2 py-1 text-sm text-slate-100"
                />
                <button type="submit" className={gameButtonClass({ tone: "indigo", size: "sm" })}>
                  {t("unActions.menu.say")}
                </button>
              </form>
            ) : (
              menu.map((kind) => (
                <button
                  key={kind.type === "Physical" ? kind.data : kind.type}
                  type="button"
                  role="menuitem"
                  className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm text-slate-100 hover:bg-white/10"
                  onClick={() => {
                    if (kind.type === "Physical") {
                      perform({ type: "Physical", data: kind.data });
                    } else {
                      setSpeaking(kind.type);
                    }
                  }}
                >
                  <span aria-hidden className="text-lg">
                    {kind.type === "Physical"
                      ? PHYSICAL_ICONS[kind.data]
                      : "\u{1F4AC}"}
                  </span>
                  {label(kind)}
                </button>
              ))
            )}
          </motion.div>
        )}
      </AnimatePresence>
      <button
        type="button"
        aria-expanded={open}
        aria-label={t(open ? "unActions.menu.close" : "unActions.menu.open")}
        onClick={() => {
          setOpen((value) => !value);
          setSpeaking(null);
          setText("");
        }}
        className={gameButtonClass({ tone: "indigo", size: "md", className: "text-xl" })}
      >
        {"\u{1F3AD}"}
      </button>
    </div>
  );
}
