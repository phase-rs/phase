import { useEffect, useMemo, useRef, useState } from "react";
import type { ParsedDeck, DeckEntry } from "../../services/deckParser";
import { detectAndParseDeck, exportDeck, resolveCommander } from "../../services/deckParser";
import type { ExportFormat } from "../../services/deckParser";
import type { DeckCompatibilityResult, UnsupportedCard } from "../../services/deckCompatibility";
import {
  sideboardPolicyForFormat,
  type SideboardPolicy,
} from "../../services/engineRuntime";
import type { GameFormat } from "../../adapter/types";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";

import { MoveList } from "./MoveList";

/**
 * Map the lowercase deck-builder format string (e.g. "standard", "commander")
 * to the engine's `GameFormat` PascalCase identifier. Derived from the
 * engine-authored FORMAT_REGISTRY so adding a format is automatic here.
 */
function mapToEngineFormat(format: string | undefined): GameFormat | null {
  if (!format) return null;
  const lower = format.toLowerCase();
  const match = FORMAT_REGISTRY.find((m) => m.format.toLowerCase() === lower);
  return match?.format ?? null;
}

/**
 * Used only when the deck's format string doesn't resolve to a known
 * GameFormat (e.g. user-imported "casual" labels). Constructed formats are
 * the common case for unfamiliar labels, so Limited(15) is the right default.
 */
const FALLBACK_CONSTRUCTED_POLICY: SideboardPolicy = { type: "Limited", data: 15 };

interface DeckListProps {
  deck: ParsedDeck;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onMoveCard: (name: string, from: "main" | "sideboard") => void;
  onImport: (deck: ParsedDeck) => void;
  onCardHover?: (cardName: string | null) => void;
  warnings?: string[];
  format?: string;
  compatibility?: DeckCompatibilityResult | null;
  onChooseArt?: (cardName: string, x: number, y: number) => void;
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
  onMoveCard,
  onImport,
  onCardHover,
  warnings = [],
  format,
  compatibility,
  onChooseArt,
}: DeckListProps) {
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [showPasteModal, setShowPasteModal] = useState(false);
  const [pasteText, setPasteText] = useState("");
  const [showExportModal, setShowExportModal] = useState(false);
  const [exportFormat, setExportFormat] = useState<ExportFormat>("dck");
  const [copied, setCopied] = useState(false);
  const [viewMode, setViewMode] = useState<"main" | "sideboard">("main");
  const mainTotal = totalCards(deck.main);
  const sideTotal = totalCards(deck.sideboard);
  const mainGroups = groupByType(deck.main);

  // CR 100.4a: Ask the engine for the format's sideboard policy rather than
  // hardcoding 15. The engine is the single authority for format rules; the
  // frontend only renders what the engine tells it.
  const [sideboardPolicy, setSideboardPolicy] = useState<SideboardPolicy>(
    FALLBACK_CONSTRUCTED_POLICY,
  );
  useEffect(() => {
    const engineFormat = mapToEngineFormat(format);
    if (!engineFormat) {
      setSideboardPolicy(FALLBACK_CONSTRUCTED_POLICY);
      return;
    }
    let cancelled = false;
    sideboardPolicyForFormat(engineFormat)
      .then((policy) => {
        if (!cancelled) setSideboardPolicy(policy);
      })
      .catch(() => {
        if (!cancelled) setSideboardPolicy(FALLBACK_CONSTRUCTED_POLICY);
      });
    return () => {
      cancelled = true;
    };
  }, [format]);

  const { sideboardTitle, sideboardWarning, hideSideboard } = useMemo(() => {
    switch (sideboardPolicy.type) {
      case "Forbidden":
        return { sideboardTitle: "", sideboardWarning: undefined, hideSideboard: true };
      case "Unlimited":
        return {
          sideboardTitle: `Sideboard (${sideTotal})`,
          sideboardWarning: undefined,
          hideSideboard: false,
        };
      case "Limited": {
        const max = sideboardPolicy.data;
        return {
          sideboardTitle: `Sideboard (${sideTotal}/${max})`,
          sideboardWarning:
            sideTotal > max ? `Sideboard exceeds ${max}-card limit` : undefined,
          hideSideboard: false,
        };
      }
    }
  }, [sideboardPolicy, sideTotal]);

  useEffect(() => {
    if (hideSideboard && viewMode === "sideboard") setViewMode("main");
  }, [hideSideboard, viewMode]);

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
    const parsed = await resolveCommander(detectAndParseDeck(content));
    onImport(parsed);
    // Reset file input so same file can be re-imported
    if (fileInputRef.current) fileInputRef.current.value = "";
  };

  const handlePasteImport = async () => {
    if (!pasteText.trim()) return;
    const parsed = await resolveCommander(detectAndParseDeck(pasteText));
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
      <div className="mb-2 flex items-center justify-between gap-2 border-b border-white/8 pb-2">
        <div className="min-w-0">
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">Current List</div>
        </div>
        <div className="flex shrink-0 gap-1">
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

      {/* Section selector: tab pair for Main / Sideboard. Full-width and
          prominent so the sideboard view is discoverable even on the
          narrow 256px right panel. Hidden when the format forbids a
          sideboard (Commander/Brawl). */}
      {!hideSideboard ? (
        <div className="mb-2 grid grid-cols-2 gap-1 rounded-xl border border-white/10 bg-black/18 p-1">
          <button
            onClick={() => setViewMode("main")}
            className={
              viewMode === "main"
                ? "rounded-lg bg-white/14 px-2 py-1 text-xs font-semibold text-white"
                : "rounded-lg px-2 py-1 text-xs text-slate-300 hover:bg-white/6"
            }
          >
            Main ({mainTotal})
          </button>
          <button
            onClick={() => setViewMode("sideboard")}
            className={
              viewMode === "sideboard"
                ? "rounded-lg bg-white/14 px-2 py-1 text-xs font-semibold text-white"
                : "rounded-lg px-2 py-1 text-xs text-slate-300 hover:bg-white/6"
            }
          >
            Sideboard ({sideTotal})
          </button>
        </div>
      ) : (
        <h3 className="mb-2 text-sm font-bold text-white">
          Main Deck ({mainTotal} cards)
        </h3>
      )}

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

      {/* Main and sideboard share this column; the header toggle (mirroring
          the "Show Browser" pattern) flips between them so the sideboard
          can't be pushed off-screen by a long main deck. The sideboard
          toggle itself is hidden for Commander/Brawl (SideboardPolicy::Forbidden). */}
      <div>
        {viewMode === "main"
          ? (["Creatures", "Spells", "Lands"] as const).map((group) => (
              <MoveList
                key={group}
                title={group}
                entries={mainGroups[group]}
                section="main"
                onRemove={onRemoveCard}
                onMove={onMoveCard}
                onCardHover={onCardHover}
                unsupportedMap={unsupportedMap}
                onChooseArt={onChooseArt}
              />
            ))
          : !hideSideboard && (
              <MoveList
                title={sideboardTitle}
                entries={deck.sideboard}
                section="sideboard"
                onRemove={onRemoveCard}
                onMove={onMoveCard}
                onCardHover={onCardHover}
                unsupportedMap={unsupportedMap}
                alwaysShow
                emptyHint="Hover a main-deck card and click → to move it here."
                warning={sideboardWarning}
                onChooseArt={onChooseArt}
              />
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
