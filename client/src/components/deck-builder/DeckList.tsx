import { useMemo, useRef, useState } from "react";
import type { ParsedDeck, DeckEntry } from "../../services/deckParser";
import { detectAndParseDeck, exportDeck } from "../../services/deckParser";
import type { ExportFormat } from "../../services/deckParser";
import type { DeckCompatibilityResult, UnsupportedCard, ParsedItem } from "../../services/deckCompatibility";

interface DeckListProps {
  deck: ParsedDeck;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onImport: (deck: ParsedDeck) => void;
  onCardHover?: (cardName: string | null) => void;
  warnings?: string[];
  format?: string;
  compatibility?: DeckCompatibilityResult | null;
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

const CATEGORY_COLORS: Record<string, string> = {
  keyword: "text-sky-400",
  ability: "text-violet-400",
  trigger: "text-amber-400",
  static: "text-teal-400",
  replacement: "text-pink-400",
  cost: "text-orange-400",
};

function ParseItemPill({ item, depth = 0 }: { item: ParsedItem; depth?: number }) {
  return (
    <>
      <span
        className={`inline-flex items-center gap-1 rounded px-1 py-px text-[9px] leading-tight ${
          item.supported
            ? "bg-emerald-500/15 text-emerald-300"
            : "bg-rose-500/15 text-rose-300"
        }`}
        title={item.source_text ?? item.label}
      >
        <span className={`font-semibold uppercase ${CATEGORY_COLORS[item.category] ?? "text-gray-400"}`}>
          {item.category.slice(0, 3)}
        </span>
        <span>{item.label}</span>
        {!item.supported && <span className="text-rose-400">&#x2717;</span>}
      </span>
      {item.children?.map((child, i) => (
        <ParseItemPill key={`${depth}-${i}`} item={child} depth={depth + 1} />
      ))}
    </>
  );
}

function CardEntryRow({
  entry,
  section,
  onRemove,
  onCardHover,
  unsupported,
}: {
  entry: DeckEntry;
  section: "main" | "sideboard";
  onRemove: (name: string, section: "main" | "sideboard") => void;
  onCardHover?: (cardName: string | null) => void;
  unsupported?: UnsupportedCard;
}) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div>
      <div
        className="group flex items-center justify-between py-0.5 text-sm"
        onMouseEnter={() => onCardHover?.(entry.name)}
        onMouseLeave={() => onCardHover?.(null)}
      >
        <span className={unsupported ? "text-amber-200/80" : "text-gray-300"}>
          <span className="mr-1 text-gray-500">{entry.count}x</span>
          {entry.name}
          {unsupported && (
            <button
              onClick={(e) => { e.stopPropagation(); setExpanded((v) => !v); }}
              className="ml-1 inline-flex h-3.5 w-3.5 items-center justify-center rounded-sm bg-amber-500/80 text-[8px] font-bold leading-none text-black"
              title={`${unsupported.gaps.length} unsupported mechanic(s) — click to expand`}
            >
              !
            </button>
          )}
        </span>
        <button
          onClick={() => onRemove(entry.name, section)}
          className="invisible ml-2 h-5 w-5 rounded text-xs text-red-400 hover:bg-red-900/40 group-hover:visible"
          title={`Remove one ${entry.name}`}
        >
          -
        </button>
      </div>
      {expanded && unsupported && (
        <div className="mb-1.5 ml-4 mt-0.5 rounded-lg border border-white/6 bg-black/30 px-2 py-1.5">
          {unsupported.oracle_text && (
            <div className="mb-1.5 font-mono text-[10px] leading-snug text-slate-400 italic">
              {unsupported.oracle_text}
            </div>
          )}
          <div className="flex flex-wrap gap-1">
            {(unsupported.parse_details ?? []).map((item, i) => (
              <ParseItemPill key={i} item={item} />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function SectionList({
  title,
  entries,
  section,
  onRemove,
  onCardHover,
  unsupportedMap,
}: {
  title: string;
  entries: DeckEntry[];
  section: "main" | "sideboard";
  onRemove: (name: string, section: "main" | "sideboard") => void;
  onCardHover?: (cardName: string | null) => void;
  unsupportedMap?: Map<string, UnsupportedCard>;
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
          unsupported={unsupportedMap?.get(entry.name)}
        />
      ))}
    </div>
  );
}

const FORMAT_DISPLAY_ORDER = ["standard", "pioneer", "modern", "legacy", "vintage", "pauper", "commander"] as const;

const FORMAT_LABELS: Record<string, string> = {
  standard: "STD",
  pioneer: "PIO",
  modern: "MOD",
  legacy: "LEG",
  vintage: "VIN",
  pauper: "PAU",
  commander: "CMD",
};

const LEGALITY_STYLES: Record<string, string> = {
  legal: "bg-emerald-600/70 text-emerald-100",
  banned: "bg-red-600/70 text-red-100",
  not_legal: "bg-gray-600/40 text-gray-500",
};

export function DeckList({
  deck,
  onRemoveCard,
  onImport,
  onCardHover,
  warnings = [],
  format: _format,
  compatibility,
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

  const unsupportedMap = useMemo(() => {
    const map = new Map<string, UnsupportedCard>();
    for (const card of compatibility?.coverage?.unsupported_cards ?? []) {
      map.set(card.name, card);
    }
    return map;
  }, [compatibility?.coverage?.unsupported_cards]);

  const handleFileImport = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const content = await file.text();
    const parsed = detectAndParseDeck(content);
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
    <div className="flex flex-col">
      <div className="mb-2 flex items-center justify-between border-b border-white/8 pb-2">
        <div>
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">Current List</div>
          <h3 className="mt-1 text-sm font-bold text-white">
            Main Deck ({mainTotal} cards)
          </h3>
        </div>
        <div className="flex gap-1">
          <button
            onClick={() => setShowPasteModal(true)}
            className="rounded-xl border border-white/8 bg-black/18 px-2 py-1 text-xs text-gray-300 hover:bg-white/6"
            title="Import deck from text (MTGA or .dck format)"
          >
            Import
          </button>
          <button
            onClick={() => setShowExportModal(true)}
            disabled={mainTotal === 0}
            className="rounded-xl border border-white/8 bg-black/18 px-2 py-1 text-xs text-gray-300 hover:bg-white/6 disabled:opacity-40"
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
            className="rounded-xl border border-amber-300/18 bg-amber-400/8 px-2 py-1 text-xs text-amber-200"
            >
              {w}
            </div>
          ))}
        </div>
      )}

      {/* Format legality & coverage */}
      {compatibility && (
        <div className="mb-3 space-y-2 border-b border-white/8 pb-3">
          {compatibility.format_legality && (
            <div>
              <div className="mb-1 text-[10px] uppercase tracking-wider text-gray-500">Format Legality</div>
              <div className="flex flex-wrap gap-1">
                {FORMAT_DISPLAY_ORDER.map((fmt) => {
                  const status = compatibility.format_legality?.[fmt] ?? "not_legal";
                  return (
                    <span
                      key={fmt}
                      className={`rounded px-1.5 py-0.5 text-[9px] font-semibold leading-tight ${LEGALITY_STYLES[status] ?? LEGALITY_STYLES.not_legal}`}
                      title={`${fmt}: ${status.replace("_", " ")}`}
                    >
                      {FORMAT_LABELS[fmt] ?? fmt}
                    </span>
                  );
                })}
              </div>
            </div>
          )}
          {compatibility.coverage && (
            <div>
              <div className="mb-1 text-[10px] uppercase tracking-wider text-gray-500">Engine Coverage</div>
              <div className="flex items-center gap-2">
                <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-gray-700">
                  <div
                    className={`h-full rounded-full ${
                      compatibility.coverage.unsupported_cards.length === 0
                        ? "bg-emerald-500"
                        : "bg-orange-500"
                    }`}
                    style={{ width: `${compatibility.coverage.total_unique > 0 ? (compatibility.coverage.supported_unique / compatibility.coverage.total_unique) * 100 : 0}%` }}
                  />
                </div>
                <span
                  className="shrink-0 text-[10px] text-gray-400"
                  title={
                    compatibility.coverage.unsupported_cards.length === 0
                      ? "All cards fully supported"
                      : `Unsupported:\n${compatibility.coverage.unsupported_cards.map((c) => `${c.name}: ${c.gaps.join(", ")}`).join("\n")}`
                  }
                >
                  {compatibility.coverage.supported_unique}/{compatibility.coverage.total_unique}
                </span>
              </div>
            </div>
          )}
        </div>
      )}

      {/* Main deck grouped by type */}
      <div>
        {(["Creatures", "Spells", "Lands"] as const).map((group) => (
          <SectionList
            key={group}
            title={group}
            entries={mainGroups[group]}
            section="main"
            onRemove={onRemoveCard}
            onCardHover={onCardHover}
            unsupportedMap={unsupportedMap}
          />
        ))}

        {/* Sideboard */}
        {deck.sideboard.length > 0 && (
          <div className="mt-3 border-t border-white/8 pt-2">
            <SectionList
              title={`Sideboard (${sideTotal})`}
              entries={deck.sideboard}
              section="sideboard"
              onRemove={onRemoveCard}
              onCardHover={onCardHover}
              unsupportedMap={unsupportedMap}
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
          <div className="relative z-10 w-full max-w-md rounded-[22px] border border-white/10 bg-[#0b1020]/96 p-6 shadow-2xl backdrop-blur-md">
            <h3 className="mb-3 text-sm font-bold text-white">Import Deck</h3>
            <textarea
              value={pasteText}
              onChange={(e) => setPasteText(e.target.value)}
              placeholder="Paste deck list (MTGA or .dck format)..."
              rows={10}
              className="mb-3 w-full rounded-[16px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-white placeholder-gray-500 focus:border-white/20 focus:outline-none"
              autoFocus
            />
            <div className="flex justify-between">
              <button
                onClick={() => fileInputRef.current?.click()}
                className="rounded-xl border border-white/8 bg-black/18 px-3 py-1.5 text-xs text-gray-300 hover:bg-white/6"
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
