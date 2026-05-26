import { motion } from "framer-motion";
import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { MoveList } from "../deck-builder/MoveList";
import type { DeckCardCount, MatchScore } from "../../adapter/types";
import type { DeckEntry } from "../../services/deckParser";

interface DeckPoolEntry {
  card: { name: string };
  count: number;
}

export interface BetweenGamesSideboardModalProps {
  /** The local player's deck pool, containing both registered (pre-match)
   *  and current (last-submitted) main + sideboard partitions. */
  pool: {
    registered_main: DeckPoolEntry[];
    registered_sideboard: DeckPoolEntry[];
    current_main: DeckPoolEntry[];
    current_sideboard: DeckPoolEntry[];
  };
  gameNumber: number;
  score: MatchScore;
  onSubmit: (main: DeckCardCount[], sideboard: DeckCardCount[]) => void;
}

/**
 * Convert engine `PlayerDeckPool` entries (`{card: {name}, count}`) into
 * `DeckEntry` shape (`{name, count}`) used by `MoveList`.
 */
function poolToEntries(pool: DeckPoolEntry[]): DeckEntry[] {
  return pool.map((e) => ({ name: e.card.name, count: e.count }));
}

function entriesToCounts(entries: DeckEntry[]): DeckCardCount[] {
  return entries
    .filter((e) => e.count > 0)
    .map(({ name, count }) => ({ name, count }));
}

function totalCards(entries: DeckEntry[]): number {
  return entries.reduce((sum, e) => sum + e.count, 0);
}

/**
 * Atomically relocate one copy of `name` from `source` to `target`. Preserves
 * the combined multiset `source ∪ target` by construction — this is the
 * partition invariant that matches the engine's pool-equality check in
 * `handle_submit_sideboard` (`match_flow.rs`).
 */
function moveOne(
  source: DeckEntry[],
  target: DeckEntry[],
  name: string,
): { source: DeckEntry[]; target: DeckEntry[] } | null {
  const sourceEntry = source.find((e) => e.name === name);
  if (!sourceEntry) return null;
  const targetEntry = target.find((e) => e.name === name);
  const nextSource =
    sourceEntry.count <= 1
      ? source.filter((e) => e.name !== name)
      : source.map((e) => (e.name === name ? { ...e, count: e.count - 1 } : e));
  const nextTarget = targetEntry
    ? target.map((e) => (e.name === name ? { ...e, count: e.count + 1 } : e))
    : [...target, { count: 1, name }];
  return { source: nextSource, target: nextTarget };
}

/**
 * BO3 between-games sideboarding modal.
 *
 * This is a **partition UI**, not an add/remove UI. Cards can only be moved
 * between the main deck and sideboard — the combined card pool is invariant,
 * matching the engine's pool-equality check. Reset restores the pre-match
 * registered partition. Submit dispatches the engine action; the engine is
 * the single authority on whether the submission is valid.
 */
export function BetweenGamesSideboardModal({
  pool,
  gameNumber,
  score,
  onSubmit,
}: BetweenGamesSideboardModalProps) {
  const { t } = useTranslation("multiplayer");
  const [drafts, setDrafts] = useState<{ main: DeckEntry[]; side: DeckEntry[] }>(
    () => ({
      main: poolToEntries(pool.current_main),
      side: poolToEntries(pool.current_sideboard),
    }),
  );

  const moveCard = useCallback(
    (name: string, from: "main" | "sideboard") => {
      setDrafts((prev) => {
        if (from === "main") {
          const next = moveOne(prev.main, prev.side, name);
          return next ? { main: next.source, side: next.target } : prev;
        }
        const next = moveOne(prev.side, prev.main, name);
        return next ? { main: next.target, side: next.source } : prev;
      });
    },
    [],
  );

  const reset = useCallback(() => {
    setDrafts({
      main: poolToEntries(pool.registered_main),
      side: poolToEntries(pool.registered_sideboard),
    });
  }, [pool.registered_main, pool.registered_sideboard]);

  const mainTotal = totalCards(drafts.main);
  const sideTotal = totalCards(drafts.side);
  const registeredMainTotal = useMemo(
    () => pool.registered_main.reduce((sum, e) => sum + e.count, 0),
    [pool.registered_main],
  );
  const canSubmit = mainTotal === registeredMainTotal;

  const handleSubmit = () => {
    if (!canSubmit) return;
    onSubmit(entriesToCounts(drafts.main), entriesToCounts(drafts.side));
  };

  const drawsLabel =
    score.draws > 0 ? t("sideboardModal.draws", { count: score.draws }) : "";

  return (
    <div className="fixed inset-0 z-50 overflow-y-auto px-2 py-2 lg:px-4 lg:py-6">
      <div className="absolute inset-0 bg-[radial-gradient(circle_at_top,rgba(31,41,55,0.55),rgba(2,6,23,0.92)_58%,rgba(2,6,23,0.98))]" />
      <div className="relative flex min-h-full items-center justify-center pb-[env(safe-area-inset-bottom)] pt-[env(safe-area-inset-top)]">
        <motion.div
          role="dialog"
          aria-modal="true"
          aria-labelledby="sideboarding-title"
          className="card-scale-reset relative z-10 flex w-full max-w-4xl flex-col overflow-hidden rounded-[14px] lg:rounded-[28px] border border-white/10 bg-[#0b1020]/94 shadow-[0_32px_90px_rgba(0,0,0,0.48)] backdrop-blur-md"
          initial={{ opacity: 0, y: 18, scale: 0.98 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          transition={{ duration: 0.24, ease: "easeOut" }}
        >
          <header className="modal-header-compact border-b border-white/10">
            <div className="modal-eyebrow uppercase tracking-[0.24em] text-slate-500">
              {t("sideboardModal.game", { number: gameNumber })}
            </div>
            <h2 id="sideboarding-title" className="font-semibold text-white">
              {t("sideboardModal.title")}
            </h2>
            <p className="modal-subtitle max-w-2xl text-slate-400">
              {t("sideboardModal.matchScore", {
                p0: score.p0_wins,
                p1: score.p1_wins,
                draws: drawsLabel,
              })}
            </p>
          </header>

          <div className="grid flex-1 grid-cols-1 gap-4 px-3 py-3 md:grid-cols-2 lg:px-5 lg:py-5">
            <MoveList
              section="main"
              title={t("sideboardModal.main", {
                count: mainTotal,
                total: registeredMainTotal,
              })}
              entries={drafts.main}
              onMove={moveCard}
              alwaysShow
              emptyHint={t("sideboardModal.moveFromSideboard")}
            />
            <MoveList
              section="sideboard"
              title={t("sideboardModal.sideboard", { count: sideTotal })}
              entries={drafts.side}
              onMove={moveCard}
              alwaysShow
              emptyHint={t("sideboardModal.moveFromMain")}
            />
          </div>

          <footer className="flex items-center justify-between gap-3 border-t border-white/10 bg-black/15 px-3 py-2 lg:px-6 lg:py-4">
            <span
              className={`text-xs ${canSubmit ? "text-emerald-300" : "text-amber-300"}`}
              role="status"
              aria-live="polite"
            >
              {canSubmit
                ? t("sideboardModal.matchesRegistered", {
                    total: registeredMainTotal,
                  })
                : t("sideboardModal.submitDisabled", {
                    count: mainTotal,
                    total: registeredMainTotal,
                  })}
            </span>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={reset}
                aria-label={t("sideboardModal.resetAria")}
                className="rounded px-3 py-1.5 text-sm text-slate-300 hover:bg-white/8 hover:text-white"
              >
                {t("sideboardModal.reset")}
              </button>
              <button
                type="button"
                onClick={handleSubmit}
                disabled={!canSubmit}
                aria-label={t("sideboardModal.submitAria")}
                className="rounded bg-emerald-600 px-4 py-1.5 text-sm font-semibold text-white enabled:hover:bg-emerald-500 disabled:cursor-not-allowed disabled:opacity-50"
              >
                {t("sideboardModal.submitDeck")}
              </button>
            </div>
          </footer>
        </motion.div>
      </div>
    </div>
  );
}
