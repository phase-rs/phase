import { useCallback, useEffect, useMemo, useState } from "react";

import { getCardPrintings } from "../../services/scryfall.ts";
import type { PrintingEntry } from "../../services/scryfall.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { ModalPanelShell } from "../ui/ModalPanelShell";

interface PrintingPickerModalProps {
  cardName: string;
  oracleId: string;
  onCardHover?: (cardName: string | null, scryfallId?: string) => void;
  onClose: () => void;
}

const INITIAL_PAGE_SIZE = 30;

export function PrintingPickerModal({
  cardName,
  oracleId,
  onCardHover,
  onClose,
}: PrintingPickerModalProps) {
  const [printings, setPrintings] = useState<PrintingEntry[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [visibleCount, setVisibleCount] = useState(INITIAL_PAGE_SIZE);
  const [query, setQuery] = useState("");

  const currentOverride = usePreferencesStore((s) => s.artOverrides[oracleId]);
  const setArtOverride = usePreferencesStore((s) => s.setArtOverride);
  const clearArtOverride = usePreferencesStore((s) => s.clearArtOverride);

  useEffect(() => {
    getCardPrintings(oracleId)
      .then((data) => setPrintings(data))
      .catch(() => setPrintings([]))
      .finally(() => setLoading(false));
  }, [oracleId]);

  const handleSelect = useCallback(
    (printing: PrintingEntry) => {
      setArtOverride(oracleId, {
        scryfallId: printing.id,
        setCode: printing.set,
        collectorNumber: printing.collector_number,
      });
      onClose();
    },
    [oracleId, setArtOverride, onClose],
  );

  const handleUseDefault = useCallback(() => {
    clearArtOverride(oracleId);
    onClose();
  }, [oracleId, clearArtOverride, onClose]);

  // Filter by set name, set code, or collector number. Reset pagination when
  // the query changes so results aren't hidden behind a stale visibleCount.
  const filteredPrintings = useMemo(() => {
    if (!printings) return [];
    const q = query.trim().toLowerCase();
    if (!q) return printings;
    return printings.filter(
      (p) =>
        p.set_name.toLowerCase().includes(q)
        || p.set.toLowerCase().includes(q)
        || p.collector_number.toLowerCase().includes(q),
    );
  }, [printings, query]);

  useEffect(() => {
    setVisibleCount(INITIAL_PAGE_SIZE);
  }, [query]);

  const visiblePrintings = filteredPrintings.slice(0, visibleCount);
  const hasMore = filteredPrintings.length > visibleCount;

  return (
    <ModalPanelShell
      title="Choose Art"
      subtitle={cardName}
      onClose={onClose}
      maxWidthClassName="max-w-4xl"
      bodyClassName="overflow-y-auto p-4 sm:p-6"
    >
      {loading && (
        <div className="flex items-center justify-center py-12">
          <div className="h-8 w-8 animate-spin rounded-full border-2 border-sky-400 border-t-transparent" />
        </div>
      )}

      {!loading && (!printings || printings.length === 0) && (
        <div className="py-12 text-center text-sm text-slate-400">
          No alternate printings available for this card.
        </div>
      )}

      {!loading && printings && printings.length > 0 && (
        <>
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Filter by set or collector number…"
            aria-label="Filter printings"
            className="mb-3 w-full rounded-[16px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-white placeholder-gray-500 focus:border-white/20 focus:outline-none"
          />

          <div className="mb-4 flex items-center justify-between">
            <span className="text-xs text-slate-400">
              {filteredPrintings.length} printing{filteredPrintings.length === 1 ? "" : "s"}
              {query.trim() && ` matching “${query.trim()}”`}
            </span>
            <button
              type="button"
              onClick={handleUseDefault}
              className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-xs text-slate-200 transition hover:bg-white/10"
            >
              Use Default
            </button>
          </div>

          {filteredPrintings.length === 0 && (
            <div className="py-8 text-center text-sm text-slate-400">
              No printings match your filter.
            </div>
          )}

          <div className="grid gap-3 grid-cols-[repeat(auto-fill,minmax(140px,1fr))]">
            {visiblePrintings.map((printing) => {
              const isSelected = currentOverride?.scryfallId === printing.id;
              const imgUrl = printing.faces[0]?.normal;
              const isBorderless = printing.border_color === "borderless";
              const isExtended = printing.frame_effects.includes("extendedart");

              return (
                <button
                  key={printing.id}
                  type="button"
                  onClick={() => handleSelect(printing)}
                  onMouseEnter={() => onCardHover?.(cardName, printing.id)}
                  onMouseLeave={() => onCardHover?.(null)}
                  className={`group relative overflow-hidden rounded-xl border transition-all ${
                    isSelected
                      ? "border-sky-400 ring-2 ring-sky-400/40"
                      : "border-white/10 hover:border-white/25"
                  }`}
                >
                  {imgUrl ? (
                    <img
                      src={imgUrl}
                      alt={`${cardName} — ${printing.set_name} #${printing.collector_number}`}
                      className="aspect-[5/7] w-full object-cover"
                      loading="lazy"
                    />
                  ) : (
                    <div className="flex aspect-[5/7] w-full items-center justify-center bg-slate-800 text-xs text-slate-500">
                      No image
                    </div>
                  )}

                  <div className="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black via-black/80 to-transparent px-2 pb-2 pt-6">
                    <div className="truncate text-[10px] font-medium text-white">
                      {printing.set_name}
                    </div>
                    <div className="flex items-center gap-1 text-[9px] text-slate-400">
                      <span className="uppercase">{printing.set}</span>
                      <span>#{printing.collector_number}</span>
                      {(isBorderless || isExtended) && (
                        <span className="ml-auto rounded bg-fuchsia-500/20 px-1 text-fuchsia-300">
                          {isBorderless ? "Borderless" : "Extended"}
                        </span>
                      )}
                    </div>
                  </div>

                  {isSelected && (
                    <div className="absolute right-1.5 top-1.5 flex h-5 w-5 items-center justify-center rounded-full bg-sky-500 text-xs text-white">
                      ✓
                    </div>
                  )}
                </button>
              );
            })}
          </div>

          {hasMore && (
            <div className="mt-4 flex justify-center">
              <button
                type="button"
                onClick={() => setVisibleCount((c) => c + INITIAL_PAGE_SIZE)}
                className="rounded-lg border border-white/10 bg-white/5 px-4 py-2 text-sm text-slate-200 transition hover:bg-white/10"
              >
                Show more ({filteredPrintings.length - visibleCount} remaining)
              </button>
            </div>
          )}
        </>
      )}
    </ModalPanelShell>
  );
}
