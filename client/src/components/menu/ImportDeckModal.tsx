import { useRef, useState } from "react";
import { createPortal } from "react-dom";
import { motion, AnimatePresence } from "framer-motion";
import { useTranslation } from "react-i18next";

import { menuButtonClass } from "./buttonStyles";
import { STORAGE_KEY_PREFIX, listSavedDeckNames, stampDeckMeta } from "../../constants/storage";
import { deriveImportedDeckName, detectAndParseDeck, resolveCommander } from "../../services/deckParser";
import { fetchDeckFromUrl } from "../../services/deckUrlImport";
import { useAppNotificationStore } from "../../stores/appToastStore";

// Frontend-authored error messages from deckUrlImport.ts arrive as translation
// keys prefixed `importDeck.`. Worker-authored messages flow through as-is
// (server pass-through, per client/src/i18n/README.md).
const I18N_KEY_PREFIX = "importDeck.";

type ImportTab = "paste" | "url" | "file";

interface ImportDeckModalProps {
  open: boolean;
  onClose: () => void;
  onImported: (name: string, deckNames: string[]) => void;
}

const GENERIC_IMPORTED_NAMES = new Set(["Imported Deck", "Untitled Deck"]);

function uniqueDeckName(baseName: string, existingNames: string[]): string {
  const existing = new Set(existingNames);
  if (!existing.has(baseName)) return baseName;

  for (let i = 2; ; i++) {
    const candidate = `${baseName} ${i}`;
    if (!existing.has(candidate)) return candidate;
  }
}

function resolveImportDeckName(
  manualName: string,
  content: string,
  deck: Awaited<ReturnType<typeof resolveCommander>>,
  fallbackName?: string,
): string {
  const trimmedManual = manualName.trim();
  if (trimmedManual) return uniqueDeckName(trimmedManual, listSavedDeckNames());

  const derivedName = deriveImportedDeckName(content, deck);
  const baseName =
    fallbackName && GENERIC_IMPORTED_NAMES.has(derivedName)
      ? fallbackName
      : derivedName;
  return uniqueDeckName(baseName, listSavedDeckNames());
}

export function ImportDeckModal({ open, onClose, onImported }: ImportDeckModalProps) {
  const { t } = useTranslation("menu");
  const showNotification = useAppNotificationStore((s) => s.showNotification);
  const [tab, setTab] = useState<ImportTab>("paste");
  const [pasteText, setPasteText] = useState("");
  const [urlText, setUrlText] = useState("");
  const [urlError, setUrlError] = useState<string | null>(null);
  const [urlLoading, setUrlLoading] = useState(false);
  const [deckName, setDeckName] = useState("");
  const fileInputRef = useRef<HTMLInputElement>(null);

  const finishImport = (name: string) => {
    onImported(name, listSavedDeckNames());
    resetAndClose();
    showNotification({
      title: t("importDeck.importedSuccessTitle"),
      description: t("importDeck.importedSuccessDescription", { name }),
    });
  };

  const handlePasteImport = async () => {
    if (!pasteText.trim()) return;
    const deck = await resolveCommander(detectAndParseDeck(pasteText));
    const name = resolveImportDeckName(deckName, pasteText, deck);
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(deck));
    stampDeckMeta(name);
    finishImport(name);
  };

  const handleUrlImport = async () => {
    const trimmed = urlText.trim();
    if (!trimmed || urlLoading) return;
    setUrlError(null);
    setUrlLoading(true);
    try {
      const content = await fetchDeckFromUrl(trimmed);
      const deck = await resolveCommander(detectAndParseDeck(content));
      const name = resolveImportDeckName(deckName, content, deck);
      localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(deck));
      stampDeckMeta(name);
      finishImport(name);
    } catch (err) {
      const raw = err instanceof Error ? err.message : t("importDeck.errorGeneric");
      setUrlError(raw.startsWith(I18N_KEY_PREFIX) ? t(raw) : raw);
    } finally {
      setUrlLoading(false);
    }
  };

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = async () => {
      const content = reader.result as string;
      const deck = await resolveCommander(detectAndParseDeck(content));
      const fallbackName = file.name.replace(/\.(dck|dec|txt)$/i, "");
      const name = resolveImportDeckName("", content, deck, fallbackName);
      localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(deck));
      stampDeckMeta(name);
      finishImport(name);
    };
    reader.readAsText(file);
    e.target.value = "";
  };

  const resetAndClose = () => {
    setPasteText("");
    setUrlText("");
    setUrlError(null);
    setUrlLoading(false);
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
            <h2 className="text-center text-xl font-bold text-white">{t("importDeck.title")}</h2>

            {/* Tabs */}
            <div className="flex">
              <button className={TAB_CLASS(tab === "paste")} onClick={() => setTab("paste")}>
                {t("importDeck.tabPaste")}
              </button>
              <button className={TAB_CLASS(tab === "url")} onClick={() => setTab("url")}>
                {t("importDeck.tabUrl")}
              </button>
              <button className={TAB_CLASS(tab === "file")} onClick={() => setTab("file")}>
                {t("importDeck.tabFile")}
              </button>
            </div>

            {tab === "paste" && (
              <div className="flex flex-col gap-3">
                <input
                  type="text"
                  value={deckName}
                  onChange={(e) => setDeckName(e.target.value)}
                  placeholder={t("importDeck.deckNamePlaceholder")}
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
                  disabled={!pasteText.trim()}
                  className={menuButtonClass({
                    tone: "amber",
                    size: "md",
                    disabled: !pasteText.trim(),
                    className: "w-full font-bold",
                  })}
                >
                  {t("importDeck.import")}
                </button>
              </div>
            )}

            {tab === "url" && (
              <div className="flex flex-col gap-3">
                <input
                  type="text"
                  value={deckName}
                  onChange={(e) => setDeckName(e.target.value)}
                  placeholder={t("importDeck.deckNamePlaceholder")}
                  className="rounded-xl border border-white/25 bg-white/8 px-3 py-2 text-sm text-white placeholder-white/30 outline-none backdrop-blur-sm focus:border-amber-300/70"
                />
                <input
                  type="url"
                  value={urlText}
                  onChange={(e) => {
                    setUrlText(e.target.value);
                    if (urlError) setUrlError(null);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void handleUrlImport();
                  }}
                  placeholder={t("importDeck.urlPlaceholder")}
                  className="rounded-xl border border-white/25 bg-white/8 px-3 py-2 text-sm text-white placeholder-white/30 outline-none backdrop-blur-sm focus:border-amber-300/70"
                />
                <p className="text-xs text-white/40">
                  {t("importDeck.urlHint")}
                </p>
                {urlError && <p className="text-xs text-red-400">{urlError}</p>}
                <button
                  onClick={handleUrlImport}
                  disabled={!urlText.trim() || urlLoading}
                  className={menuButtonClass({
                    tone: "amber",
                    size: "md",
                    disabled: !urlText.trim() || urlLoading,
                    className: "w-full font-bold",
                  })}
                >
                  {urlLoading ? t("importDeck.importing") : t("importDeck.import")}
                </button>
              </div>
            )}

            {tab === "file" && (
              <div className="flex flex-col items-center gap-4 py-4">
                <p className="text-sm text-white/50">
                  {t("importDeck.fileSupports")}
                </p>
                <button
                  onClick={() => fileInputRef.current?.click()}
                  className={menuButtonClass({
                    tone: "amber",
                    size: "lg",
                    className: "w-full font-bold",
                  })}
                >
                  {t("importDeck.chooseFile")}
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
              {t("common:actions.cancel")}
            </button>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
