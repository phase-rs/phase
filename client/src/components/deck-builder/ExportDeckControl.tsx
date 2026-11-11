import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { exportDeck, parsedDeckHasCards } from "../../services/deckParser";
import type { ExportFormat, ParsedDeck } from "../../services/deckParser";
import { copyText } from "../../services/copyText";
import { ModalPanelShell } from "../ui/ModalPanelShell";

interface ExportDeckControlProps {
  deck: ParsedDeck;
  /** Styles the trigger in the caller's button system, given whether it is disabled. */
  triggerClassName: (disabled: boolean) => string;
}

/** Export button plus the dialog that shows the deck as `.dck` or MTGA text. */
export function ExportDeckControl({ deck, triggerClassName }: ExportDeckControlProps) {
  const { t } = useTranslation(["deck-builder", "common"]);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [showExportModal, setShowExportModal] = useState(false);
  const [exportFormat, setExportFormat] = useState<ExportFormat>("dck");
  const [copied, setCopied] = useState(false);
  const disabled = !parsedDeckHasCards(deck);

  const exportText = showExportModal ? exportDeck(deck, exportFormat) : "";

  const handleSaveToFile = () => {
    const blob = new Blob([exportText], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = exportFormat === "mtga" ? "deck.txt" : "deck.dck";
    a.click();
    URL.revokeObjectURL(url);
  };

  const handleCopyToClipboard = async () => {
    if (!(await copyText(exportText))) return;
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleClose = () => {
    setShowExportModal(false);
    setCopied(false);
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setShowExportModal(true)}
        disabled={disabled}
        title={t("deckList.exportTitle")}
        aria-label={t("deckList.exportTitle")}
        className={triggerClassName(disabled)}
      >
        {t("deckList.export")}
      </button>
      <ModalPanelShell
        open={showExportModal}
        title={t("deckList.exportModalTitle")}
        onClose={handleClose}
        maxWidthClassName="max-w-md"
        bodyClassName="overflow-y-auto p-6"
        returnFocusRef={triggerRef}
      >
        <div className="mb-3 flex justify-end">
          <div className="flex rounded bg-gray-800 p-0.5 text-xs">
            <button
              onClick={() => { setExportFormat("dck"); setCopied(false); }}
              className={`rounded px-2 py-1 ${exportFormat === "dck" ? "bg-gray-600 text-white" : "text-gray-400 hover:text-gray-200"}`}
            >
              .dck
            </button>
            <button
              onClick={() => { setExportFormat("mtga"); setCopied(false); }}
              className={`rounded px-2 py-1 ${exportFormat === "mtga" ? "bg-gray-600 text-white" : "text-gray-400 hover:text-gray-200"}`}
            >
              MTGA
            </button>
          </div>
        </div>
        <textarea
          value={exportText}
          readOnly
          rows={12}
          className="mb-3 w-full rounded border border-gray-700 bg-gray-800 px-3 py-2 font-mono text-sm text-white focus:border-blue-500 focus:outline-none"
          onFocus={(e) => e.target.select()}
        />
        <div className="flex justify-between">
          <button
            onClick={handleSaveToFile}
            className="rounded bg-gray-700 px-3 py-1.5 text-xs text-gray-300 hover:bg-gray-600"
          >
            {t("deckList.saveToFile")}
          </button>
          <div className="flex gap-2">
            <button
              onClick={handleClose}
              className="rounded bg-gray-700 px-3 py-1.5 text-xs text-gray-300 hover:bg-gray-600"
            >
              {t("common:actions.close")}
            </button>
            <button
              onClick={handleCopyToClipboard}
              className="rounded bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-500"
            >
              {copied ? t("deckList.copied") : t("deckList.copy")}
            </button>
          </div>
        </div>
      </ModalPanelShell>
    </>
  );
}
