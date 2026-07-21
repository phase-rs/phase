import { useRef, useState } from "react";
import { createPortal } from "react-dom";
import { motion, AnimatePresence } from "framer-motion";
import { useTranslation } from "react-i18next";

import { menuButtonClass } from "./buttonStyles";
import { STORAGE_KEY_PREFIX, listSavedDeckNames, stampDeckMeta } from "../../constants/storage";
import {
  assignOathbreakerSlots,
  deriveImportedDeckName,
  detectAndParseDeck,
  expandParsedDeck,
  parsedDeckHasCards,
  resolveCommander,
  type ParsedDeck,
} from "../../services/deckParser";
import { fetchDeckFromUrl } from "../../services/deckUrlImport";
import {
  isCardCommanderEligibleForFormat,
  signatureSpellSelectionPolicy,
} from "../../services/engineRuntime";
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

interface PendingOathbreakerImport {
  deck: ParsedDeck;
  name: string;
  oathbreakerCandidates: string[];
  signatureSpellCandidates: string[];
  oathbreaker: string;
  signatureSpell: string;
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

function initialSignatureSpell(deck: ParsedDeck, candidates: string[]): string {
  const declaredSignatureSpell = deck.signature_spell?.[0];
  if (declaredSignatureSpell && candidates.includes(declaredSignatureSpell)) {
    return declaredSignatureSpell;
  }
  return candidates.length === 1 ? candidates[0] : "";
}

export function ImportDeckModal({ open, onClose, onImported }: ImportDeckModalProps) {
  const { t } = useTranslation("menu");
  const showNotification = useAppNotificationStore((s) => s.showNotification);
  const [tab, setTab] = useState<ImportTab>("paste");
  const [pasteText, setPasteText] = useState("");
  const [urlText, setUrlText] = useState("");
  const [urlError, setUrlError] = useState<string | null>(null);
  const [urlLoading, setUrlLoading] = useState(false);
  const [pasteError, setPasteError] = useState<string | null>(null);
  const [pasteLoading, setPasteLoading] = useState(false);
  const [fileError, setFileError] = useState<string | null>(null);
  const [deckName, setDeckName] = useState("");
  const [importAsOathbreaker, setImportAsOathbreaker] = useState(false);
  const [pendingOathbreakerImport, setPendingOathbreakerImport] = useState<PendingOathbreakerImport | null>(null);
  const [oathbreakerSetupLoading, setOathbreakerSetupLoading] = useState(false);
  const [oathbreakerSetupError, setOathbreakerSetupError] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const finishImport = (name: string) => {
    onImported(name, listSavedDeckNames());
    resetAndClose();
    showNotification({
      title: t("importDeck.importedSuccessTitle"),
      description: t("importDeck.importedSuccessDescription", { name }),
    });
  };

  const persistImport = (name: string, deck: ParsedDeck, format?: "Oathbreaker") => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + name,
      JSON.stringify(format ? { ...deck, format } : deck),
    );
    stampDeckMeta(name);
    finishImport(name);
  };

  const signatureCandidatesFor = async (deck: ParsedDeck, oathbreaker: string): Promise<string[]> => {
    const expanded = expandParsedDeck(deck);
    const policy = await signatureSpellSelectionPolicy({
      ...expanded,
      // An exporter may already put the signature spell in its own section.
      // The engine policy accepts a main-deck candidate pool, so include that
      // declared card while asking it for the legal choices.
      main_deck: [...expanded.main_deck, ...(deck.signature_spell ?? [])],
      commander: [oathbreaker],
      selected_format: "Oathbreaker",
    });
    return policy.type === "Required" ? policy.data.candidates : [];
  };

  const stageOathbreakerImport = async (deck: ParsedDeck, name: string) => {
    const candidateNames = Array.from(new Set([
      ...deck.main.map((entry) => entry.name),
      ...(deck.commander ?? []),
    ]));
    const eligibility = await Promise.all(
      candidateNames.map(async (candidate) => ({
        candidate,
        eligible: await isCardCommanderEligibleForFormat(candidate, "Oathbreaker"),
      })),
    );
    const oathbreakerCandidates = eligibility
      .filter(({ eligible }) => eligible)
      .map(({ candidate }) => candidate);
    if (oathbreakerCandidates.length === 0) {
      setOathbreakerSetupError(t("importDeck.errorNoOathbreaker"));
      return;
    }

    const oathbreaker = oathbreakerCandidates.length === 1 ? oathbreakerCandidates[0] : "";
    const signatureSpellCandidates = oathbreaker
      ? await signatureCandidatesFor(deck, oathbreaker)
      : [];
    if (oathbreaker && signatureSpellCandidates.length === 0) {
      setOathbreakerSetupError(t("importDeck.errorNoSignatureSpell"));
      return;
    }
    setPendingOathbreakerImport({
      deck,
      name,
      oathbreakerCandidates,
      signatureSpellCandidates,
      oathbreaker,
      signatureSpell: initialSignatureSpell(deck, signatureSpellCandidates),
    });
  };

  const stageImport = async (content: string, fallbackName?: string): Promise<boolean> => {
    const deck = await resolveCommander(detectAndParseDeck(content));
    if (!parsedDeckHasCards(deck)) return false;

    const name = resolveImportDeckName(deckName, content, deck, fallbackName);
    if (!importAsOathbreaker) {
      persistImport(name, deck);
      return true;
    }

    await stageOathbreakerImport(deck, name);
    return true;
  };

  const handleOathbreakerChange = async (oathbreaker: string) => {
    const pending = pendingOathbreakerImport;
    if (!pending) return;

    setOathbreakerSetupLoading(true);
    setOathbreakerSetupError(null);
    try {
      const signatureSpellCandidates = oathbreaker
        ? await signatureCandidatesFor(pending.deck, oathbreaker)
        : [];
      setPendingOathbreakerImport((current) => current && {
        ...current,
        oathbreaker,
        signatureSpellCandidates,
        signatureSpell: initialSignatureSpell(current.deck, signatureSpellCandidates),
      });
    } catch {
      setOathbreakerSetupError(t("importDeck.errorGeneric"));
    } finally {
      setOathbreakerSetupLoading(false);
    }
  };

  const confirmOathbreakerImport = () => {
    const pending = pendingOathbreakerImport;
    if (!pending?.oathbreaker || !pending.signatureSpell) return;
    persistImport(
      pending.name,
      assignOathbreakerSlots(pending.deck, pending.oathbreaker, pending.signatureSpell),
      "Oathbreaker",
    );
  };

  const handlePasteImport = async () => {
    if (!pasteText.trim() || pasteLoading) return;
    setPasteError(null);
    setPasteLoading(true);
    try {
      if (!(await stageImport(pasteText))) {
        setPasteError(t("importDeck.errorNoCards"));
      }
    } catch {
      setPasteError(t("importDeck.errorGeneric"));
    } finally {
      setPasteLoading(false);
    }
  };

  const handleUrlImport = async () => {
    const trimmed = urlText.trim();
    if (!trimmed || urlLoading) return;
    setUrlError(null);
    setUrlLoading(true);
    try {
      const content = await fetchDeckFromUrl(trimmed);
      if (!(await stageImport(content))) {
        setUrlError(t("importDeck.errorNoCards"));
      }
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
      setFileError(null);
      const content = reader.result as string;
      const fallbackName = file.name.replace(/\.(dck|dec|txt)$/i, "");
      try {
        if (!(await stageImport(content, fallbackName))) {
          setFileError(t("importDeck.errorNoCards"));
        }
      } catch {
        setFileError(t("importDeck.errorGeneric"));
      }
    };
    reader.readAsText(file);
    e.target.value = "";
  };

  const resetAndClose = () => {
    setPasteText("");
    setUrlText("");
    setUrlError(null);
    setUrlLoading(false);
    setPasteError(null);
    setPasteLoading(false);
    setFileError(null);
    setDeckName("");
    setImportAsOathbreaker(false);
    setPendingOathbreakerImport(null);
    setOathbreakerSetupLoading(false);
    setOathbreakerSetupError(null);
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

            {pendingOathbreakerImport ? (
              <div className="flex flex-col gap-4">
                <div className="rounded-xl border border-purple-400/30 bg-purple-950/30 p-3">
                  <h3 className="text-sm font-semibold text-purple-100">
                    {t("importDeck.oathbreakerSetupTitle")}
                  </h3>
                  <p className="mt-1 text-xs text-purple-100/70">
                    {t("importDeck.oathbreakerSetupDescription")}
                  </p>
                </div>
                <label className="flex flex-col gap-1.5 text-sm text-white/80">
                  {t("importDeck.oathbreakerLabel")}
                  <select
                    value={pendingOathbreakerImport.oathbreaker}
                    onChange={(event) => void handleOathbreakerChange(event.target.value)}
                    className="rounded-xl border border-white/25 bg-slate-900 px-3 py-2 text-sm text-white outline-none focus:border-purple-300/70"
                  >
                    <option value="">{t("importDeck.chooseOathbreaker")}</option>
                    {pendingOathbreakerImport.oathbreakerCandidates.map((candidate) => (
                      <option key={candidate} value={candidate}>{candidate}</option>
                    ))}
                  </select>
                </label>
                <label className="flex flex-col gap-1.5 text-sm text-white/80">
                  {t("importDeck.signatureSpellLabel")}
                  <select
                    value={pendingOathbreakerImport.signatureSpell}
                    onChange={(event) => setPendingOathbreakerImport((current) => current && ({
                      ...current,
                      signatureSpell: event.target.value,
                    }))}
                    disabled={!pendingOathbreakerImport.oathbreaker || oathbreakerSetupLoading}
                    className="rounded-xl border border-white/25 bg-slate-900 px-3 py-2 text-sm text-white outline-none focus:border-purple-300/70 disabled:cursor-not-allowed disabled:opacity-50"
                  >
                    <option value="">{t("importDeck.chooseSignatureSpell")}</option>
                    {pendingOathbreakerImport.signatureSpellCandidates.map((candidate) => (
                      <option key={candidate} value={candidate}>{candidate}</option>
                    ))}
                  </select>
                </label>
                {oathbreakerSetupError && <p className="text-xs text-red-400">{oathbreakerSetupError}</p>}
                <button
                  onClick={confirmOathbreakerImport}
                  disabled={
                    oathbreakerSetupLoading
                    || !pendingOathbreakerImport.oathbreaker
                    || !pendingOathbreakerImport.signatureSpell
                  }
                  className={menuButtonClass({
                    tone: "amber",
                    size: "md",
                    disabled:
                      oathbreakerSetupLoading
                      || !pendingOathbreakerImport.oathbreaker
                      || !pendingOathbreakerImport.signatureSpell,
                    className: "w-full font-bold",
                  })}
                >
                  {oathbreakerSetupLoading
                    ? t("importDeck.importing")
                    : t("importDeck.confirmOathbreakerImport")}
                </button>
                <button
                  onClick={() => {
                    setPendingOathbreakerImport(null);
                    setOathbreakerSetupError(null);
                  }}
                  className="text-sm text-white/40 transition-colors hover:text-white/70"
                >
                  {t("importDeck.backToImport")}
                </button>
              </div>
            ) : (
              <>
                {/* Tabs */}
                <div className="flex">
                  <button className={TAB_CLASS(tab === "paste")} onClick={() => { setTab("paste"); setFileError(null); }}>
                    {t("importDeck.tabPaste")}
                  </button>
                  <button className={TAB_CLASS(tab === "url")} onClick={() => setTab("url")}>
                    {t("importDeck.tabUrl")}
                  </button>
                  <button className={TAB_CLASS(tab === "file")} onClick={() => { setTab("file"); setPasteError(null); }}>
                    {t("importDeck.tabFile")}
                  </button>
                </div>

                <label className="flex items-center gap-2 rounded-lg border border-purple-400/25 bg-purple-950/20 px-3 py-2 text-sm text-purple-100">
                  <input
                    type="checkbox"
                    checked={importAsOathbreaker}
                    onChange={(event) => {
                      setImportAsOathbreaker(event.target.checked);
                      setOathbreakerSetupError(null);
                    }}
                    className="h-4 w-4 accent-purple-400"
                  />
                  {t("importDeck.importAsOathbreaker")}
                </label>
                {oathbreakerSetupError && <p className="text-xs text-red-400">{oathbreakerSetupError}</p>}

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
                  onChange={(e) => {
                    setPasteText(e.target.value);
                    if (pasteError) setPasteError(null);
                  }}
                  placeholder={"Paste deck list here...\n\nSupports .dck, .dec, and MTGA format:\n4 Thoughtseize (THS) 107\n2 Fatal Push (KLR) 84"}
                  rows={10}
                  className="resize-none rounded-xl border border-white/25 bg-white/8 px-3 py-2 font-mono text-xs leading-relaxed text-white placeholder-white/20 outline-none backdrop-blur-sm focus:border-amber-300/70"
                />
                {pasteError && <p className="text-xs text-red-400">{pasteError}</p>}
                <button
                  onClick={handlePasteImport}
                  disabled={!pasteText.trim() || pasteLoading}
                  className={menuButtonClass({
                    tone: "amber",
                    size: "md",
                    disabled: !pasteText.trim() || pasteLoading,
                    className: "w-full font-bold",
                  })}
                >
                  {pasteLoading ? t("importDeck.importing") : t("importDeck.import")}
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
                {fileError && <p className="text-xs text-red-400">{fileError}</p>}
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
              </>
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
