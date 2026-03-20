import { useRef, useState } from "react";
import { motion, AnimatePresence } from "framer-motion";

import { menuButtonClass } from "./buttonStyles";
import { STORAGE_KEY_PREFIX, listSavedDeckNames, stampDeckMeta } from "../../constants/storage";
import { detectAndParseDeck } from "../../services/deckParser";

type ImportTab = "paste" | "file";

interface ImportDeckModalProps {
  open: boolean;
  onClose: () => void;
  onImported: (name: string, deckNames: string[]) => void;
}

export function ImportDeckModal({ open, onClose, onImported }: ImportDeckModalProps) {
  const [tab, setTab] = useState<ImportTab>("paste");
  const [pasteText, setPasteText] = useState("");
  const [deckName, setDeckName] = useState("");
  const fileInputRef = useRef<HTMLInputElement>(null);

  const handlePasteImport = () => {
    const name = deckName.trim();
    if (!name || !pasteText.trim()) return;
    const deck = detectAndParseDeck(pasteText);
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(deck));
    stampDeckMeta(name);
    const names = listSavedDeckNames();
    onImported(name, names);
    resetAndClose();
  };

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      const content = reader.result as string;
      const deck = detectAndParseDeck(content);
      const name = file.name.replace(/\.(dck|dec|txt)$/i, "");
      localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(deck));
      stampDeckMeta(name);
      const names = listSavedDeckNames();
      onImported(name, names);
      resetAndClose();
    };
    reader.readAsText(file);
    e.target.value = "";
  };

  const resetAndClose = () => {
    setPasteText("");
    setDeckName("");
    setTab("paste");
    onClose();
  };

  const TAB_CLASS = (active: boolean) =>
    `flex-1 py-2 text-sm font-medium transition-colors ${
      active
        ? "border-b-2 border-amber-400 text-amber-100"
        : "border-b border-white/10 text-white/40 hover:text-white/70"
    }`;

  return (
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
            <h2 className="text-center text-xl font-bold text-white">Import Deck</h2>

            {/* Tabs */}
            <div className="flex">
              <button className={TAB_CLASS(tab === "paste")} onClick={() => setTab("paste")}>
                Paste Text
              </button>
              <button className={TAB_CLASS(tab === "file")} onClick={() => setTab("file")}>
                From File
              </button>
            </div>

            {tab === "paste" && (
              <div className="flex flex-col gap-3">
                <input
                  type="text"
                  value={deckName}
                  onChange={(e) => setDeckName(e.target.value)}
                  placeholder="Deck name"
                  className="rounded-xl border border-white/25 bg-white/8 px-3 py-2 text-sm text-white placeholder-white/30 outline-none backdrop-blur-sm focus:border-amber-300/70"
                />
                <textarea
                  value={pasteText}
                  onChange={(e) => setPasteText(e.target.value)}
                  placeholder={"Paste deck list here...\n\nSupports .dck, .dec, and MTGA format:\n4 Thoughtseize (THS) 107\n2 Fatal Push (KLR) 84"}
                  rows={10}
                  className="resize-none rounded-xl border border-white/25 bg-white/8 px-3 py-2 font-mono text-xs leading-relaxed text-white placeholder-white/20 outline-none backdrop-blur-sm focus:border-amber-300/70"
                />
                <button
                  onClick={handlePasteImport}
                  disabled={!deckName.trim() || !pasteText.trim()}
                  className={menuButtonClass({
                    tone: "amber",
                    size: "md",
                    disabled: !deckName.trim() || !pasteText.trim(),
                    className: "w-full font-bold",
                  })}
                >
                  Import
                </button>
              </div>
            )}

            {tab === "file" && (
              <div className="flex flex-col items-center gap-4 py-4">
                <p className="text-sm text-white/50">
                  Supports .dck, .dec, .txt, and MTGA format
                </p>
                <button
                  onClick={() => fileInputRef.current?.click()}
                  className={menuButtonClass({
                    tone: "amber",
                    size: "lg",
                    className: "w-full font-bold",
                  })}
                >
                  Choose File
                </button>
                <input
                  ref={fileInputRef}
                  type="file"
                  accept=".dck,.dec,.txt"
                  onChange={handleFileChange}
                  className="hidden"
                />
              </div>
            )}

            <button
              onClick={resetAndClose}
              className="text-sm text-white/40 transition-colors hover:text-white/70"
            >
              Cancel
            </button>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
