import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { isCommanderPreconDeck, useDecks, type DeckEntry } from "../../hooks/useDecks";
import { preconExists, savePreconDeck } from "../../services/preconDecks";
import { menuButtonClass } from "./buttonStyles";
import { MenuSelect } from "../ui/MenuSelect";

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
  const { t } = useTranslation("menu");
  const { decks, status } = useDecks();
  const [query, setQuery] = useState("");
  const [typeFilter, setTypeFilter] = useState<string>(ALL_TYPES);
  // Multi-select state. Keyed by deck id (filename stem) so it survives the
  // filtered list reordering as the user types — a deck stays selected even
  // when the search query temporarily hides it.
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());

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
      if (!isCommanderPreconDeck(d)) continue;
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
        if (!isCommanderPreconDeck(d)) return false;
        if (typeFilter !== ALL_TYPES && d.type !== typeFilter) return false;
        return matchesQuery(d, q);
      })
      .sort(([, a], [, b]) => (b.releaseDate ?? "").localeCompare(a.releaseDate ?? ""))
      .slice(0, MAX_RESULTS);
  }, [decks, query, typeFilter]);

  const typeOptions = useMemo(
    () =>
      Array.from(typeCounts.entries())
        .filter(([, n]) => n > 0)
        .sort((a, b) => a[0].localeCompare(b[0])),
    [typeCounts],
  );

  const typeFilterItems = useMemo(
    () => [
      { value: ALL_TYPES, label: t("precon.allTypes", { count: totalMatches }) },
      ...typeOptions.map(([type, n]) => ({ value: type, label: `${type} (${n})` })),
    ],
    [t, totalMatches, typeOptions],
  );

  const typeFilterLabel = useMemo(() => {
    if (typeFilter === ALL_TYPES) {
      return t("precon.allTypes", { count: totalMatches });
    }
    const match = typeOptions.find(([type]) => type === typeFilter);
    return match ? `${match[0]} (${match[1]})` : typeFilter;
  }, [typeFilter, totalMatches, typeOptions, t]);

  if (!open) return null;

  const handlePick = (deck: DeckEntry) => {
    const suggested = `${deck.name} (${deck.code})`;
    const chosen = prompt(t("precon.savePrompt"), suggested);
    if (!chosen) return;
    if (preconExists(chosen) && !confirm(t("precon.overwriteConfirm", { name: chosen }))) return;
    savePreconDeck(chosen, deck);
    onImported(chosen);
    onClose();
  };

  const toggleSelected = (id: string) => {
    setSelectedIds((cur) => {
      const next = new Set(cur);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const clearSelection = () => setSelectedIds(new Set());

  // Bulk import. Auto-names every selection as `Name (CODE)` (matching the
  // single-import suggested-default), batches collision handling into one
  // confirm, and fires `onImported` exactly once at the end with the last
  // imported name so the parent's deck-list refresh + select-mode auto-pick
  // run as a single transition rather than per-deck.
  const handleImportSelected = () => {
    if (!decks || selectedIds.size === 0) return;
    const picks: Array<{ savedName: string; deck: DeckEntry }> = [];
    for (const id of selectedIds) {
      const deck = decks[id];
      if (!deck) continue;
      picks.push({ savedName: `${deck.name} (${deck.code})`, deck });
    }
    if (picks.length === 0) return;

    const conflicts = picks.filter((p) => preconExists(p.savedName));
    let overwrite = true;
    if (conflicts.length > 0) {
      const msg =
        conflicts.length === picks.length
          ? t("precon.overwriteAllConfirm", { count: picks.length })
          : t("precon.overwriteSomeConfirm", { conflicts: conflicts.length, total: picks.length });
      overwrite = confirm(msg);
    }

    let lastImported: string | null = null;
    let imported = 0;
    let skipped = 0;
    for (const { savedName, deck } of picks) {
      if (preconExists(savedName) && !overwrite) {
        skipped++;
        continue;
      }
      savePreconDeck(savedName, deck);
      lastImported = savedName;
      imported++;
    }

    if (lastImported) onImported(lastImported);
    clearSelection();
    onClose();
    if (skipped > 0) {
      // No toast system in this surface — a single alert keeps the user
      // informed without ambiguity. Fires AFTER onClose so the dialog tears
      // down first and the alert lands in the deck-list view.
      alert(t("precon.importedSkipped", { count: imported, skipped }));
    }
  };

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
              {t("precon.title")}
            </h2>
            <p className="mt-1 text-xs text-slate-400">
              {t("precon.subtitle")}
              {decks && (
                <span className="ml-1 text-slate-500">
                  {t("precon.totalCount", { count: Object.values(decks).filter(isCommanderPreconDeck).length })}
                </span>
              )}
            </p>
          </div>
          <button
            onClick={onClose}
            className="text-slate-400 transition hover:text-white"
            aria-label={t("common:actions.close")}
          >
            ✕
          </button>
        </div>

        <div className="flex flex-col gap-2 sm:flex-row">
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("precon.searchPlaceholder")}
            className="flex-1 rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white placeholder:text-slate-500 focus:border-white/30 focus:outline-none"
            autoFocus
          />
          <MenuSelect
            ariaLabel={t("precon.typeFilter")}
            label={typeFilterLabel}
            selectedValue={typeFilter}
            items={typeFilterItems}
            onSelect={setTypeFilter}
            menuLayout="dropdown"
            wrapperClassName="sm:w-52 shrink-0"
            fitContainer
            disabled={status === "loading" || status === "error"}
            className="rounded-lg border-white/10 bg-black/30 py-2 focus-visible:ring-white/30"
          />
        </div>


        <div className="flex-1 overflow-y-auto rounded-lg border border-white/5 bg-black/20">
          {status === "loading" ? (
            <div className="p-8 text-center text-sm text-slate-500">{t("precon.loadingCatalog")}</div>
          ) : status === "error" ? (
            <div className="p-8 text-center text-sm text-rose-300/90">{t("precon.loadFailed")}</div>
          ) : filtered.length === 0 ? (
            <div className="p-8 text-center text-sm text-slate-500">{t("precon.noMatch")}</div>
          ) : (
            <ul className="divide-y divide-white/5">
              {filtered.map(([id, deck]) => {
                const checked = selectedIds.has(id);
                return (
                  <li key={id} className="flex items-stretch">
                    {/* Checkbox column — toggles multi-select. Stops propagation
                        so the keyboard "space" / click here doesn't fire the
                        row's single-pick handler. */}
                    <label
                      className="flex cursor-pointer items-center pl-4 pr-2"
                      onClick={(e) => e.stopPropagation()}
                    >
                      <input
                        type="checkbox"
                        checked={checked}
                        onChange={() => toggleSelected(id)}
                        className="h-4 w-4 cursor-pointer accent-indigo-400"
                        aria-label={t("precon.selectDeck", { name: deck.name })}
                      />
                    </label>
                    {/* Row body — single-import fast path. Multi-select users
                        just click the checkbox; everyone else clicks the row. */}
                    <button
                      onClick={() => handlePick(deck)}
                      className="flex flex-1 items-center justify-between gap-3 py-2 pr-4 text-left transition hover:bg-white/5"
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
                          {t("precon.cardCount", { count: mainBoardCount(deck) })}
                          {deck.commander && deck.commander.length > 0 && t("precon.commanderSuffix")}
                        </span>
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        <div className="flex items-center justify-between gap-3 text-xs text-slate-500">
          <span className="flex flex-wrap items-center gap-3">
            {decks && filtered.length === MAX_RESULTS && (
              <span>{t("precon.showingFirst", { count: MAX_RESULTS })}</span>
            )}
            {/* Bulk-select helpers. Hidden until the catalog has loaded. */}
            {decks && filtered.length > 0 && (
              <button
                type="button"
                onClick={() =>
                  setSelectedIds((cur) => {
                    const next = new Set(cur);
                    for (const [id] of filtered) next.add(id);
                    return next;
                  })
                }
                className="text-slate-400 underline-offset-2 hover:text-white hover:underline"
              >
                {t("precon.selectAllVisible")}
              </button>
            )}
            {selectedIds.size > 0 && (
              <button
                type="button"
                onClick={clearSelection}
                className="text-slate-400 underline-offset-2 hover:text-white hover:underline"
              >
                {t("precon.clearSelection", { count: selectedIds.size })}
              </button>
            )}
          </span>
          <span className="flex items-center gap-2">
            {selectedIds.size > 0 && (
              <button
                type="button"
                onClick={handleImportSelected}
                className={menuButtonClass({ tone: "indigo", size: "sm" })}
              >
                {t("precon.importSelected", { count: selectedIds.size })}
              </button>
            )}
            <button onClick={onClose} className={menuButtonClass({ tone: "neutral", size: "sm" })}>
              {t("common:actions.close")}
            </button>
          </span>
        </div>
      </div>
    </div>
  );
}
