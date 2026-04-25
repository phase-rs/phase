import { useEffect, useMemo, useState } from "react";

import { useDecks, type DeckEntry } from "../../hooks/useDecks";
import { preconExists, savePreconDeck } from "../../services/preconDecks";
import { menuButtonClass } from "./buttonStyles";

interface PreconDeckModalProps {
  open: boolean;
  onClose: () => void;
  onImported: (name: string) => void;
}

/** Cap on rendered rows. Prevents 1000+ node lists becoming a perf cliff;
 * users narrow further with the search input. */
const MAX_RESULTS = 500;
const ALL_TYPES = "All" as const;

function matchesQuery(deck: DeckEntry, q: string): boolean {
  if (!q) return true;
  return (
    deck.name.toLowerCase().includes(q) ||
    deck.code.toLowerCase().includes(q) ||
    deck.type.toLowerCase().includes(q)
  );
}

function mainBoardCount(deck: DeckEntry): number {
  return deck.mainBoard.reduce((n, c) => n + c.count, 0);
}

/** Color the coverage % badge so a glance tells the user what to expect.
 *  Mirrors the AiOpponentConfig coverage threshold UX. */
function coverageTone(pct: number): string {
  if (pct >= 100) return "text-emerald-300";
  if (pct >= 90) return "text-lime-300";
  if (pct >= 75) return "text-amber-300";
  return "text-rose-400";
}

export function PreconDeckModal({ open, onClose, onImported }: PreconDeckModalProps) {
  const decks = useDecks();
  const [query, setQuery] = useState("");
  const [typeFilter, setTypeFilter] = useState<string>(ALL_TYPES);

  // Esc-to-close, bound only while the modal is open.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  // Per-type match counts against the current query. Drives the dropdown so
  // types with zero matches don't appear as dead-end options.
  const typeCounts = useMemo(() => {
    if (!decks) return new Map<string, number>();
    const q = query.trim().toLowerCase();
    const counts = new Map<string, number>();
    for (const d of Object.values(decks)) {
      if (!matchesQuery(d, q)) continue;
      counts.set(d.type, (counts.get(d.type) ?? 0) + 1);
    }
    return counts;
  }, [decks, query]);

  const totalMatches = useMemo(
    () => Array.from(typeCounts.values()).reduce((a, b) => a + b, 0),
    [typeCounts],
  );

  const filtered = useMemo(() => {
    if (!decks) return [];
    const q = query.trim().toLowerCase();
    return Object.entries(decks)
      .filter(([, d]) => {
        if (typeFilter !== ALL_TYPES && d.type !== typeFilter) return false;
        return matchesQuery(d, q);
      })
      .sort(([, a], [, b]) => (b.releaseDate ?? "").localeCompare(a.releaseDate ?? ""))
      .slice(0, MAX_RESULTS);
  }, [decks, query, typeFilter]);

  if (!open) return null;

  const handlePick = (deck: DeckEntry) => {
    const suggested = `${deck.name} (${deck.code})`;
    const chosen = prompt("Save preconstructed deck as:", suggested);
    if (!chosen) return;
    if (preconExists(chosen) && !confirm(`"${chosen}" already exists. Overwrite?`)) return;
    savePreconDeck(chosen, deck);
    onImported(chosen);
    onClose();
  };

  const typeOptions = Array.from(typeCounts.entries())
    .filter(([, n]) => n > 0)
    .sort((a, b) => a[0].localeCompare(b[0]));

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4 backdrop-blur-sm"
      role="dialog"
      aria-modal="true"
      aria-labelledby="precon-modal-title"
      onClick={onClose}
    >
      <div
        className="flex max-h-[80vh] w-full max-w-3xl flex-col gap-4 rounded-2xl border border-white/10 bg-[#0a0f1b] p-6 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-4">
          <div>
            <h2 id="precon-modal-title" className="text-xl font-semibold text-white">
              Preconstructed Decks
            </h2>
            <p className="mt-1 text-xs text-slate-400">
              Sourced from MTGJSON AllDeckFiles — every WotC-printed precon.
              {decks && (
                <span className="ml-1 text-slate-500">
                  · {Object.keys(decks).length} total
                </span>
              )}
            </p>
          </div>
          <button
            onClick={onClose}
            className="text-slate-400 transition hover:text-white"
            aria-label="Close"
          >
            ✕
          </button>
        </div>

        <div className="flex flex-col gap-2 sm:flex-row">
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search by name, set code, or type…"
            className="flex-1 rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white placeholder:text-slate-500 focus:border-white/30 focus:outline-none"
            autoFocus
          />
          <select
            value={typeFilter}
            onChange={(e) => setTypeFilter(e.target.value)}
            className="rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white focus:border-white/30 focus:outline-none"
          >
            <option value={ALL_TYPES}>All ({totalMatches})</option>
            {typeOptions.map(([t, n]) => (
              <option key={t} value={t}>
                {t} ({n})
              </option>
            ))}
          </select>
        </div>


        <div className="flex-1 overflow-y-auto rounded-lg border border-white/5 bg-black/20">
          {!decks ? (
            <div className="p-8 text-center text-sm text-slate-500">Loading deck catalog…</div>
          ) : filtered.length === 0 ? (
            <div className="p-8 text-center text-sm text-slate-500">No decks match.</div>
          ) : (
            <ul className="divide-y divide-white/5">
              {filtered.map(([id, deck]) => (
                <li key={id}>
                  <button
                    onClick={() => handlePick(deck)}
                    className="flex w-full items-center justify-between gap-3 px-4 py-2 text-left transition hover:bg-white/5"
                  >
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium text-white">{deck.name}</div>
                      <div className="truncate text-[11px] text-slate-500">
                        {deck.type}
                        {deck.releaseDate && <span> · {deck.releaseDate}</span>}
                        <span> · {deck.code}</span>
                      </div>
                    </div>
                    <span className="flex shrink-0 items-baseline gap-2 text-[11px]">
                      <span className={`font-semibold ${coverageTone(deck.coveragePct)}`}>
                        {deck.coveragePct}%
                      </span>
                      <span className="text-slate-600">
                        {mainBoardCount(deck)} cards
                        {deck.commander && deck.commander.length > 0 && " · cmdr"}
                      </span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="flex items-center justify-between text-xs text-slate-500">
          <span>
            {decks && filtered.length === MAX_RESULTS
              ? `Showing first ${MAX_RESULTS} — refine search to narrow.`
              : null}
          </span>
          <button onClick={onClose} className={menuButtonClass({ tone: "neutral", size: "sm" })}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
