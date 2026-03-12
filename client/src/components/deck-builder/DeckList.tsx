import { useRef, useState } from "react";
import type { ParsedDeck, DeckEntry } from "../../services/deckParser";
import { parseDeckFile, detectAndParseDeck, exportDeck } from "../../services/deckParser";
import type { ExportFormat } from "../../services/deckParser";

interface DeckListProps {
  deck: ParsedDeck;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onImport: (deck: ParsedDeck) => void;
  onCardHover?: (cardName: string | null) => void;
  warnings?: string[];
  format?: string;
}


interface GroupedEntries {
  Creatures: DeckEntry[];
  Spells: DeckEntry[];
  Lands: DeckEntry[];
}

function groupByType(entries: DeckEntry[]): GroupedEntries {
  const groups: GroupedEntries = { Creatures: [], Spells: [], Lands: [] };
  for (const entry of entries) {
    // Without full card data, we use name heuristics; actual categorization
    // will be enhanced when Scryfall data is cached.
    // For now, all go to Spells unless we integrate card type data.
    groups.Spells.push(entry);
  }
  return groups;
}

function totalCards(entries: DeckEntry[]): number {
  return entries.reduce((sum, e) => sum + e.count, 0);
}

function CardEntryRow({
  entry,
  section,
  onRemove,
  onCardHover,
}: {
  entry: DeckEntry;
  section: "main" | "sideboard";
  onRemove: (name: string, section: "main" | "sideboard") => void;
  onCardHover?: (cardName: string | null) => void;
}) {
  return (
    <div
      className="group flex items-center justify-between py-0.5 text-sm"
      onMouseEnter={() => onCardHover?.(entry.name)}
      onMouseLeave={() => onCardHover?.(null)}
    >
      <span className="text-gray-300">
        <span className="mr-1 text-gray-500">{entry.count}x</span>
        {entry.name}
      </span>
      <button
        onClick={() => onRemove(entry.name, section)}
        className="invisible ml-2 h-5 w-5 rounded text-xs text-red-400 hover:bg-red-900/40 group-hover:visible"
        title={`Remove one ${entry.name}`}
      >
        -
      </button>
    </div>
  );
}

function SectionList({
  title,
  entries,
  section,
  onRemove,
  onCardHover,
}: {
  title: string;
  entries: DeckEntry[];
  section: "main" | "sideboard";
  onRemove: (name: string, section: "main" | "sideboard") => void;
  onCardHover?: (cardName: string | null) => void;
}) {
  if (entries.length === 0) return null;
  const count = totalCards(entries);

  return (
    <div className="mb-2">
      <div className="mb-1 flex justify-between text-xs font-semibold uppercase text-gray-500">
        <span>{title}</span>
        <span>({count})</span>
      </div>
      {entries.map((entry) => (
        <CardEntryRow
          key={entry.name}
          entry={entry}
          section={section}
          onRemove={onRemove}
          onCardHover={onCardHover}
        />
      ))}
    </div>
  );
}

export function DeckList({
  deck,
  onRemoveCard,
  onImport,
  onCardHover,
  warnings = [],
  format: _format,
}: DeckListProps) {
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [showPasteModal, setShowPasteModal] = useState(false);
  const [pasteText, setPasteText] = useState("");
  const [showExportModal, setShowExportModal] = useState(false);
  const [exportFormat, setExportFormat] = useState<ExportFormat>("dck");
  const [copied, setCopied] = useState(false);
  const mainTotal = totalCards(deck.main);
  const sideTotal = totalCards(deck.sideboard);
  const mainGroups = groupByType(deck.main);

  const handleFileImport = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const content = await file.text();
    const parsed = parseDeckFile(content);
    onImport(parsed);
    // Reset file input so same file can be re-imported
    if (fileInputRef.current) fileInputRef.current.value = "";
  };

  const handlePasteImport = () => {
    if (!pasteText.trim()) return;
    const parsed = detectAndParseDeck(pasteText);
    onImport(parsed);
    setPasteText("");
    setShowPasteModal(false);
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
    <div className="flex h-full flex-col">
      {/* Header with total */}
      <div className="mb-2 flex items-center justify-between border-b border-gray-700 pb-2">
        <h3 className="text-sm font-bold text-white">
          Deck ({mainTotal} cards)
        </h3>
        <div className="flex gap-1">
          <button
            onClick={() => setShowPasteModal(true)}
            className="rounded bg-gray-700 px-2 py-1 text-xs text-gray-300 hover:bg-gray-600"
            title="Import deck from text (MTGA or .dck format)"
          >
            Import
          </button>
          <button
            onClick={() => setShowExportModal(true)}
            disabled={mainTotal === 0}
            className="rounded bg-gray-700 px-2 py-1 text-xs text-gray-300 hover:bg-gray-600 disabled:opacity-40"
            title="Export deck"
          >
            Export
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

      {/* Warnings */}
      {warnings.length > 0 && (
        <div className="mb-2 space-y-0.5">
          {warnings.map((w) => (
            <div
              key={w}
              className="rounded bg-yellow-900/30 px-2 py-1 text-xs text-yellow-400"
            >
              {w}
            </div>
          ))}
        </div>
      )}

      {/* Main deck grouped by type */}
      <div className="flex-1 overflow-y-auto">
        {(["Creatures", "Spells", "Lands"] as const).map((group) => (
          <SectionList
            key={group}
            title={group}
            entries={mainGroups[group]}
            section="main"
            onRemove={onRemoveCard}
            onCardHover={onCardHover}
          />
        ))}

        {/* Sideboard */}
        {deck.sideboard.length > 0 && (
          <div className="mt-3 border-t border-gray-700 pt-2">
            <SectionList
              title={`Sideboard (${sideTotal})`}
              entries={deck.sideboard}
              section="sideboard"
              onRemove={onRemoveCard}
              onCardHover={onCardHover}
            />
          </div>
        )}
      </div>

      {/* Paste import modal */}
      {showPasteModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/60"
            onClick={() => setShowPasteModal(false)}
          />
          <div className="relative z-10 w-full max-w-md rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700">
            <h3 className="mb-3 text-sm font-bold text-white">Import Deck</h3>
            <textarea
              value={pasteText}
              onChange={(e) => setPasteText(e.target.value)}
              placeholder="Paste deck list (MTGA or .dck format)..."
              rows={10}
              className="mb-3 w-full rounded border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:border-blue-500 focus:outline-none"
              autoFocus
            />
            <div className="flex justify-between">
              <button
                onClick={() => fileInputRef.current?.click()}
                className="rounded bg-gray-700 px-3 py-1.5 text-xs text-gray-300 hover:bg-gray-600"
              >
                From File
              </button>
              <div className="flex gap-2">
                <button
                  onClick={() => {
                    setPasteText("");
                    setShowPasteModal(false);
                  }}
                  className="rounded bg-gray-700 px-3 py-1.5 text-xs text-gray-300 hover:bg-gray-600"
                >
                  Cancel
                </button>
                <button
                  onClick={handlePasteImport}
                  disabled={!pasteText.trim()}
                  className="rounded bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-500 disabled:opacity-40"
                >
                  Parse
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
              <h3 className="text-sm font-bold text-white">Export Deck</h3>
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
                Save to File
              </button>
              <div className="flex gap-2">
                <button
                  onClick={() => {
                    setShowExportModal(false);
                    setCopied(false);
                  }}
                  className="rounded bg-gray-700 px-3 py-1.5 text-xs text-gray-300 hover:bg-gray-600"
                >
                  Close
                </button>
                <button
                  onClick={handleCopyToClipboard}
                  className="rounded bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-500"
                >
                  {copied ? "Copied!" : "Copy"}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}


