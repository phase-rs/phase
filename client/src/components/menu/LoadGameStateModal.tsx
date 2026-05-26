import { useRef, useState } from "react";
import { createPortal } from "react-dom";
import { motion, AnimatePresence } from "framer-motion";
import { useTranslation } from "react-i18next";

import { menuButtonClass } from "./buttonStyles";
import type { GameState } from "../../adapter/types";
import {
  gameStateFromImportText,
  readImportFile,
} from "../../services/gameStateImport";

type LoadTab = "paste" | "file";

interface LoadGameStateModalProps {
  open: boolean;
  onClose: () => void;
  /** Invoked with a validated GameState once parsing succeeds. The caller owns
   *  persistence + navigation into the game. */
  onLoaded: (state: GameState) => void;
}

export function LoadGameStateModal({ open, onClose, onLoaded }: LoadGameStateModalProps) {
  const { t } = useTranslation("menu");
  const [tab, setTab] = useState<LoadTab>("paste");
  const [pasteText, setPasteText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const resetAndClose = () => {
    setPasteText("");
    setError(null);
    setTab("paste");
    onClose();
  };

  const handleParsed = (text: string) => {
    const state = gameStateFromImportText(text);
    if (typeof state === "string") {
      setError(state);
      return;
    }
    onLoaded(state);
    resetAndClose();
  };

  const handlePasteLoad = () => {
    if (!pasteText.trim()) return;
    setError(null);
    handleParsed(pasteText);
  };

  const handleFileChange = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    setError(null);
    try {
      handleParsed(await readImportFile(file));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : t("loadGameState.readFailed"));
    }
  };

  const TAB_CLASS = (active: boolean) =>
    `flex-1 py-2 text-sm font-medium transition-colors ${
      active
        ? "border-b-2 border-amber-400 text-amber-100"
        : "border-b border-white/10 text-white/40 hover:text-white/70"
    }`;

  return createPortal(
    <AnimatePresence>
      {open && (
        <motion.div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.2 }}
          onClick={resetAndClose}
        >
          <motion.div
            className="flex w-[95vw] max-w-md flex-col gap-4 rounded-2xl border border-slate-600/40 bg-slate-800/95 p-6 shadow-2xl"
            style={{ boxShadow: "0 0 40px rgba(0,0,0,0.5), 0 0 80px rgba(0,0,0,0.3)" }}
            initial={{ scale: 0.85, opacity: 0, y: 20 }}
            animate={{ scale: 1, opacity: 1, y: 0 }}
            exit={{ scale: 0.85, opacity: 0, y: 20 }}
            transition={{ type: "spring", stiffness: 400, damping: 25 }}
            onClick={(e) => e.stopPropagation()}
          >
            <h2 className="text-center text-xl font-bold text-white">{t("loadState.title")}</h2>
            <p className="-mt-2 text-center text-sm text-white/50">{t("loadState.subtitle")}</p>

            {/* Tabs */}
            <div className="flex">
              <button className={TAB_CLASS(tab === "paste")} onClick={() => setTab("paste")}>
                {t("loadState.tabPaste")}
              </button>
              <button className={TAB_CLASS(tab === "file")} onClick={() => setTab("file")}>
                {t("loadState.tabFile")}
              </button>
            </div>

            {tab === "paste" && (
              <div className="flex flex-col gap-3">
                <textarea
                  value={pasteText}
                  onChange={(e) => setPasteText(e.target.value)}
                  placeholder={t("loadState.pastePlaceholder")}
                  rows={10}
                  className="resize-none rounded-xl border border-white/25 bg-white/8 px-3 py-2 font-mono text-xs leading-relaxed text-white placeholder-white/20 outline-none backdrop-blur-sm focus:border-amber-300/70"
                />
                <button
                  onClick={handlePasteLoad}
                  disabled={!pasteText.trim()}
                  className={menuButtonClass({
                    tone: "amber",
                    size: "md",
                    disabled: !pasteText.trim(),
                    className: "w-full font-bold",
                  })}
                >
                  {t("loadState.load")}
                </button>
              </div>
            )}

            {tab === "file" && (
              <div className="flex flex-col items-center gap-4 py-4">
                <p className="text-sm text-white/50">{t("loadState.fileSupports")}</p>
                <button
                  onClick={() => fileInputRef.current?.click()}
                  className={menuButtonClass({
                    tone: "amber",
                    size: "lg",
                    className: "w-full font-bold",
                  })}
                >
                  {t("loadState.chooseFile")}
                </button>
                <input
                  ref={fileInputRef}
                  type="file"
                  accept=".json,.txt,.zip,application/json,text/plain,application/zip"
                  onChange={handleFileChange}
                  className="hidden"
                />
              </div>
            )}

            {error && (
              <div className="rounded-lg bg-red-900/50 px-3 py-2 text-center text-xs text-red-300">
                {error}
              </div>
            )}

            <button
              onClick={resetAndClose}
              className="text-sm text-white/40 transition-colors hover:text-white/70"
            >
              {t("common:actions.cancel")}
            </button>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
