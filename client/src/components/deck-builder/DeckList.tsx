import { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ParsedDeck, DeckEntry } from "../../services/deckParser";
import {
  detectAndParseDeck,
  exportDeck,
  parsedDeckHasCards,
  resolveCommander,
} from "../../services/deckParser";
import type { ExportFormat } from "../../services/deckParser";
import type { DeckCompatibilityResult, UnsupportedCard } from "../../services/deckCompatibility";
import type { ScryfallCard } from "../../services/scryfall";

import { MoveList } from "./MoveList";
import { mouseHoverPreview } from "./hoverPreview";
import { groupAccent, groupKey, groupOrder, groupTitleKey, type GroupMode } from "./deckGrouping";
import { isMaybeboardPolicy, useSideboardPolicy } from "./useSideboardPolicy";

interface DeckListProps {
  deck: ParsedDeck;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onMoveCard: (name: string, from: "main" | "sideboard") => void;
  onImport: (deck: ParsedDeck) => void;
  onCardHover?: (cardName: string | null) => void;
  format?: string;
  compatibility?: DeckCompatibilityResult | null;
  onChooseArt?: (cardName: string, x: number, y: number) => void;
  /** When provided, each main-deck row of a commander-eligible card renders a
   *  crown button that promotes the card to commander. Format-gated by the
   *  parent (DeckBuilder); MoveList/CardEntryRow stay format-agnostic. */
  onSetAsCommander?: (name: string) => void;
  isCommanderEligible?: (name: string) => boolean;
  /** Touch path for art selection — forwarded to each row's ✦ badge. */
  onOpenArtPicker?: (name: string) => void;
  /** Designated commander(s). Rendered as a pinned section above the section
   *  tabs (mirroring the visual stack's Commander lane) so the commander stays
   *  visible/removable in list view — on mobile the Info-panel CommanderPanel
   *  is on a different tab. Empty in non-commander formats, so the section
   *  self-hides. */
  commanders?: string[];
  /** Demotes a commander back into the main deck. Paired with `commanders`. */
  onRemoveCommander?: (name: string) => void;
  /** Card data used to classify each main-deck entry into its group. */
  cardDataCache: Map<string, ScryfallCard>;
  /** Whether the main deck is sub-grouped by card type or by color. */
  groupMode: GroupMode;
}


function totalCards(entries: DeckEntry[]): number {
  return entries.reduce((sum, e) => sum + e.count, 0);
}


export function DeckList({
  deck,
  onRemoveCard,
  onMoveCard,
  onImport,
  onCardHover,
  format,
  compatibility,
  onChooseArt,
  onSetAsCommander,
  isCommanderEligible,
  onOpenArtPicker,
  commanders = [],
  onRemoveCommander,
  cardDataCache,
  groupMode,
}: DeckListProps) {
  const { t } = useTranslation("deck-builder");
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [showPasteModal, setShowPasteModal] = useState(false);
  const [pasteText, setPasteText] = useState("");
  const [pasteError, setPasteError] = useState<string | null>(null);
  const [pasteLoading, setPasteLoading] = useState(false);
  const [showExportModal, setShowExportModal] = useState(false);
  const [exportFormat, setExportFormat] = useState<ExportFormat>("dck");
  const [copied, setCopied] = useState(false);
  const [viewMode, setViewMode] = useState<"main" | "sideboard">("main");
  const mainTotal = totalCards(deck.main);
  const sideTotal = totalCards(deck.sideboard);
  const mainGroups = useMemo(() => {
    const buckets = new Map<string, DeckEntry[]>();
    for (const entry of deck.main) {
      const key = groupKey(groupMode, cardDataCache.get(entry.name));
      const bucket = buckets.get(key);
      if (bucket) bucket.push(entry);
      else buckets.set(key, [entry]);
    }
    return buckets;
  }, [deck.main, groupMode, cardDataCache]);

  // CR 100.4a: Ask the engine for the format's sideboard policy rather than
  // hardcoding 15. The engine is the single authority for format rules; the
  // frontend only renders what the engine tells it. Forbidden-sideboard
  // formats (Commander/Brawl) repurpose the second section as a builder-only
  // "Maybeboard" staging area — see useSideboardPolicy.
  const sideboardPolicy = useSideboardPolicy(format);
  const isMaybeboard = isMaybeboardPolicy(sideboardPolicy);
  const mainName = t("deckList.mainName");
  const sectionName = isMaybeboard ? t("deckList.maybeboardName") : t("deckList.sideboardName");

  const { sideboardTitle, sideboardWarning } = useMemo(() => {
    switch (sideboardPolicy.type) {
      case "Forbidden":
        return {
          sideboardTitle: t("deckList.maybeboard", { count: sideTotal }),
          sideboardWarning: undefined,
        };
      case "Unlimited":
        return {
          sideboardTitle: t("deckList.sideboardUnlimited", { count: sideTotal }),
          sideboardWarning: undefined,
        };
      case "Limited": {
        const max = sideboardPolicy.data;
        return {
          sideboardTitle: t("deckList.sideboardLimited", { count: sideTotal, max }),
          sideboardWarning:
            sideTotal > max ? t("deckList.sideboardExceeds", { max }) : undefined,
        };
      }
    }
  }, [sideboardPolicy, sideTotal, t]);

  const unsupportedMap = useMemo(() => {
    const map = new Map<string, UnsupportedCard>();
    for (const card of compatibility?.coverage?.unsupported_cards ?? []) {
      map.set(card.name, card);
    }
    return map;
  }, [compatibility?.coverage?.unsupported_cards]);

  const importParsedDeck = async (content: string): Promise<boolean> => {
    const parsed = await resolveCommander(detectAndParseDeck(content));
    if (!parsedDeckHasCards(parsed)) {
      setPasteError(t("deckList.parseError"));
      return false;
    }
    onImport(parsed);
    setPasteText("");
    setPasteError(null);
    setShowPasteModal(false);
    return true;
  };

  const handleFileImport = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    setPasteError(null);
    setPasteLoading(true);
    try {
      const content = await file.text();
      await importParsedDeck(content);
    } finally {
      setPasteLoading(false);
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  };

  const handlePasteImport = async () => {
    if (!pasteText.trim() || pasteLoading) return;
    setPasteError(null);
    setPasteLoading(true);
    try {
      await importParsedDeck(pasteText);
    } finally {
      setPasteLoading(false);
    }
  };

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
    await navigator.clipboard.writeText(exportText);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="flex flex-col">
      <div className="mb-2 flex items-center justify-between gap-2 border-b border-white/8 pb-2">
        <div className="min-w-0">
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">{t("deckList.currentList")}</div>
        </div>
        <div className="flex shrink-0 gap-1">
          <button
            onClick={() => {
              setPasteError(null);
              setShowPasteModal(true);
            }}
            className="rounded-xl border border-white/8 bg-black/18 px-2 py-1 text-xs text-gray-300 hover:bg-white/6"
            title={t("deckList.importTitle")}
          >
            {t("deckList.import")}
          </button>
          <button
            onClick={() => setShowExportModal(true)}
            disabled={mainTotal === 0}
            className="rounded-xl border border-white/8 bg-black/18 px-2 py-1 text-xs text-gray-300 hover:bg-white/6 disabled:opacity-40"
            title={t("deckList.exportTitle")}
          >
            {t("deckList.export")}
          </button>
          <input
            ref={fileInputRef}
            type="file"
            accept=".dck,.dec"
            onChange={handleFileImport}
            className="hidden"
          />
        </div>
      </div>

      {/* Commander section: pinned above the tabs (not inside one) so it's
          visible on whichever tab is active. CR 903.5a — the commander lives in
          the command zone, not the deck, so it's shown separately and excluded
          from the main count. Self-hides outside commander formats (commanders
          is empty). Designation still happens via the ♛ crown on eligible rows;
          this section only displays + demotes the current commander(s). */}
      {commanders.length > 0 && (
        <div className="mb-2 rounded-xl border border-fuchsia-300/25 bg-fuchsia-500/8 p-2">
          <div className="mb-1 flex items-center gap-1.5 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-fuchsia-200/80">
            <span aria-hidden="true">♛</span>
            {t("deckList.commanderHeading")}
          </div>
          {commanders.map((name) => (
            <div
              key={name}
              data-card-name={name.toLowerCase()}
              className="group flex items-center justify-between py-0.5 text-sm"
            >
              <span
                className={`text-fuchsia-50 ${onCardHover ? "cursor-pointer" : ""}`}
                onClick={() => onCardHover?.(name)}
                {...mouseHoverPreview(onCardHover, name)}
              >
                {name}
              </span>
              {onRemoveCommander && (
                <button
                  type="button"
                  onClick={() => onRemoveCommander(name)}
                  className="ml-2 h-9 w-9 rounded text-red-400 hover:bg-red-900/40 lg:h-7 lg:w-7"
                  aria-label={t("deckList.removeCommander", { name })}
                  title={t("deckList.removeCommander", { name })}
                >
                  -
                </button>
              )}
            </div>
          ))}
        </div>
      )}

      {/* Section selector: tab pair for Main / Sideboard (or Main / Maybeboard
          in Commander/Brawl). Full-width and prominent so the second section is
          discoverable even on the narrow 256px right panel — and so cards moved
          there are always recoverable, rather than vanishing into a hidden
          section. */}
      <div className="mb-2 grid grid-cols-2 gap-1 rounded-xl border border-white/10 bg-black/18 p-1">
        <button
          onClick={() => setViewMode("main")}
          className={
            viewMode === "main"
              ? "rounded-lg bg-white/14 px-2 py-1 text-xs font-semibold text-white"
              : "rounded-lg px-2 py-1 text-xs text-slate-300 hover:bg-white/6"
          }
        >
          {t("deckList.mainTab", { count: mainTotal })}
        </button>
        <button
          onClick={() => setViewMode("sideboard")}
          className={
            viewMode === "sideboard"
              ? "rounded-lg bg-white/14 px-2 py-1 text-xs font-semibold text-white"
              : "rounded-lg px-2 py-1 text-xs text-slate-300 hover:bg-white/6"
          }
        >
          {isMaybeboard
            ? t("deckList.maybeboardTab", { count: sideTotal })
            : t("deckList.sideboardTab", { count: sideTotal })}
        </button>
      </div>

      {/* Validation warnings now pin as a banner at the Deck-surface level (so
          they show in both list and stack views); format legality & engine
          coverage live in StatsPanel. The per-card unsupported `!` flags remain
          inline via unsupportedMap below. */}

      {/* Main and the second section share this column; the tab toggle flips
          between them so neither can be pushed off-screen by a long deck. Each
          row's move button is labelled with its destination (→ Sideboard /
          → Maybeboard / → Main) so the move target is explicit on touch. */}
      <div>
        {viewMode === "main"
          ? groupOrder(groupMode).map((key) => (
              <MoveList
                key={key}
                title={t(`deckList.${groupTitleKey(groupMode, key)}`)}
                accent={groupAccent(key)}
                entries={mainGroups.get(key) ?? []}
                section="main"
                onRemove={onRemoveCard}
                onMove={onMoveCard}
                onCardHover={onCardHover}
                unsupportedMap={unsupportedMap}
                onChooseArt={onChooseArt}
                onSetAsCommander={onSetAsCommander}
                isCommanderEligible={isCommanderEligible}
                density="comfortable"
                onOpenArtPicker={onOpenArtPicker}
                moveTargetLabel={sectionName}
              />
            ))
          : (
              <MoveList
                title={sideboardTitle}
                entries={deck.sideboard}
                section="sideboard"
                onRemove={onRemoveCard}
                onMove={onMoveCard}
                onCardHover={onCardHover}
                unsupportedMap={unsupportedMap}
                alwaysShow
                emptyHint={
                  isMaybeboard
                    ? t("deckList.maybeboardEmptyHint")
                    : t("deckList.sideboardEmptyHint")
                }
                warning={sideboardWarning}
                onChooseArt={onChooseArt}
                density="comfortable"
                onOpenArtPicker={onOpenArtPicker}
                moveTargetLabel={mainName}
              />
            )}
      </div>

      {/* Paste import modal */}
      {showPasteModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/60"
            onClick={() => {
              setPasteError(null);
              setShowPasteModal(false);
            }}
          />
          <div className="relative z-10 w-full max-w-md rounded-[22px] border border-white/10 bg-[#0b1020]/96 p-6 shadow-2xl backdrop-blur-md">
            <h3 className="mb-3 text-sm font-bold text-white">{t("deckList.importModalTitle")}</h3>
            <textarea
              value={pasteText}
              onChange={(e) => {
                setPasteText(e.target.value);
                if (pasteError) setPasteError(null);
              }}
              placeholder={t("deckList.pastePlaceholder")}
              rows={10}
              className="mb-3 w-full rounded-[16px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-white placeholder-gray-500 focus:border-white/20 focus:outline-none"
              autoFocus
            />
            {pasteError && <p className="mb-3 text-xs text-red-400">{pasteError}</p>}
            <div className="flex justify-between">
              <button
                onClick={() => fileInputRef.current?.click()}
                disabled={pasteLoading}
                className="rounded-xl border border-white/8 bg-black/18 px-3 py-1.5 text-xs text-gray-300 hover:bg-white/6 disabled:opacity-40"
              >
                {t("deckList.fromFile")}
              </button>
              <div className="flex gap-2">
                <button
                  onClick={() => {
                    setPasteText("");
                    setPasteError(null);
                    setShowPasteModal(false);
                  }}
                  disabled={pasteLoading}
                  className="rounded bg-gray-700 px-3 py-1.5 text-xs text-gray-300 hover:bg-gray-600 disabled:opacity-40"
                >
                  {t("common:actions.cancel")}
                </button>
                <button
                  onClick={handlePasteImport}
                  disabled={!pasteText.trim() || pasteLoading}
                  className="rounded bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-500 disabled:opacity-40"
                >
                  {pasteLoading ? t("deckList.parsing") : t("deckList.parse")}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Export modal */}
      {showExportModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/60"
            onClick={() => {
              setShowExportModal(false);
              setCopied(false);
            }}
          />
          <div className="relative z-10 w-full max-w-md rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700">
            <div className="mb-3 flex items-center justify-between">
              <h3 className="text-sm font-bold text-white">{t("deckList.exportModalTitle")}</h3>
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
              autoFocus
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
                  onClick={() => {
                    setShowExportModal(false);
                    setCopied(false);
                  }}
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
          </div>
        </div>
      )}
    </div>
  );
}
