import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { FEED_REGISTRY } from "../../data/feedRegistry";
import { useDeckFolders } from "../../hooks/useDeckFolders";
import {
  buildFolderIndex,
  buildPoolSections,
  candidateInPool,
  emptyPoolSelection,
  isPoolSelectionEmpty,
  poolGroupState,
  setPoolDeckIncluded,
  setPoolGroupIncluded,
  summarizePoolSelection,
  type PoolCandidate,
  type RandomPoolKind,
} from "../../services/randomPoolSelection";
import { loadDeckMetaMap } from "../../constants/storage";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { BracketFilter } from "./BracketFilter";

interface RandomPoolTreeProps {
  pool: RandomPoolKind;
  /**
   * Legal-catalog base: the full pool before this selection applies. The tree
   * always lists every toggleable deck, but when `baseIds` is present the
   * header also reports how many selected decks survive the sibling
   * restriction — the seat pickers' "Random (N)" counts show the combined
   * result after soft preferences narrow it further.
   */
  items: PoolCandidate[];
  /**
   * Ids of the draw base the selection intersects with (the AI bracket pool
   * from `resolveAiDrawBase`). When present the header reports playable
   * decks alongside selected ones and warns when the intersection is empty —
   * the case where draws silently fall back to the full base. Omit for pools
   * with no sibling restriction (immediate human picks).
   */
  baseIds?: ReadonlySet<string>;
  /**
   * Show the shared AI bracket filter chips under the header so the human
   * pool's bracket restriction is visible and adjustable where it applies.
   * The AI setup has its own chips row, so this stays human-only.
   */
  showBracketFilter?: boolean;
  /**
   * Complete toggleable universe. `items` is the filtered display view (deck
   * search narrows it), but a folder toggle means "select/unselect ALL decks
   * in that folder" — its override-clearing must cover hidden members too, or
   * a searched-out explicit include lingers invisibly. Omit only when the
   * display view already is the whole universe; the AI surfaces and the
   * deferred human pool pass the complete raw catalog (including decks the
   * legal display view hides) so overrides on unlisted decks cannot survive
   * a folder-wide toggle.
   */
  allItems?: PoolCandidate[];
}

const COLLAPSED: ReadonlySet<string> = new Set();

function feedNameOf(feedId: string): string {
  return FEED_REGISTRY.find((feed) => feed.id === feedId)?.name ?? feedId;
}

/**
 * One random pool's folder/deck curation tree, shared by the human pool
 * (MyDecks select surfaces) and the AI pool (single-player setup, Host Game).
 * Groups start collapsed — the common action is toggling a whole folder from
 * its header; expanding flips individual decks against the group.
 */
export function RandomPoolTree({ pool, items, baseIds, showBracketFilter, allItems }: RandomPoolTreeProps) {
  const { t } = useTranslation("menu");
  const selection = usePreferencesStore((s) =>
    pool === "ai" ? s.aiPoolSelection : s.humanPoolSelection,
  );
  const setSelection = usePreferencesStore((s) =>
    pool === "ai" ? s.setAiPoolSelection : s.setHumanPoolSelection,
  );
  const bracketFilter = usePreferencesStore((s) => s.aiBracketFilter);
  const setBracketFilter = usePreferencesStore((s) => s.setAiBracketFilter);
  const { folders } = useDeckFolders();
  const [open, setOpen] = useState(false);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(COLLAPSED);

  // `folders` identity refreshes on every library change (our own writes,
  // other tabs, cloud-sync replaces), so the index — folders plus a fresh
  // metadata read — never goes stale.
  const index = useMemo(() => buildFolderIndex(folders, loadDeckMetaMap()), [folders]);
  const sections = useMemo(
    () => buildPoolSections(items, { folders, feedNameOf }, index),
    [items, folders, index],
  );
  const included = useMemo(() => {
    const map = new Map<string, boolean>();
    for (const item of items) map.set(item.id, candidateInPool(item, selection, index));
    return map;
  }, [items, selection, index]);
  // Complete per-group membership for toggles: displayed members drive the
  // rows, but the toggle action covers every true member (see `allItems`).
  const completeMembers = useMemo(() => {
    const map = new Map<string, string[]>();
    if (!allItems) return map;
    for (const section of buildPoolSections(allItems, { folders, feedNameOf }, index)) {
      map.set(section.key, section.members.map((member) => member.id));
    }
    return map;
  }, [allItems, folders, index]);
  const rows = useMemo(
    () =>
      sections.map((section) => {
        const memberIds = section.members.map((member) => member.id);
        let inCount = 0;
        for (const id of memberIds) {
          if (included.get(id)) inCount += 1;
        }
        return {
          section,
          memberIds,
          toggleIds: completeMembers.get(section.key) ?? memberIds,
          inCount,
          state: poolGroupState(memberIds, section.key, selection),
        };
      }),
    [sections, completeMembers, included, selection],
  );
  const summary = useMemo(
    () => summarizePoolSelection(items, selection, baseIds, index),
    [items, selection, baseIds, index],
  );

  if (items.length === 0) return null;

  const total = items.length;
  const { selected: selectedCount, playable: playableCount, baseSize } = summary;
  // Warn-plus-fallback: the draw layer serves the full base when the selection
  // matches nothing, so say so on the always-visible header (the tree itself
  // starts collapsed). The conflict case is subtler and worse: decks ARE
  // selected, but none survive the sibling restriction, so draws would
  // silently ignore the curation entirely.
  const fellBack = selectedCount === 0;
  const baseConflict = baseIds !== undefined && baseSize > 0 && selectedCount > 0 && playableCount === 0;
  const toggleExpanded = (key: string) => {
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={() => setOpen((was) => !was)}
          aria-expanded={open}
          className="flex min-w-0 flex-1 items-center gap-2 text-left"
        >
          <Chevron expanded={open} />
          <span className="truncate text-xs text-slate-400">
            {t(pool === "ai" ? "randomPool.titleAi" : "randomPool.titleHuman")}
          </span>
          <span className={`shrink-0 text-xs font-medium ${fellBack || baseConflict ? "text-amber-300" : "text-slate-300"}`}>
            {t("randomPool.count", { selected: selectedCount, total })}
            {baseIds !== undefined && playableCount !== selectedCount && (
              <> · {t("randomPool.playable", { playable: playableCount })}</>
            )}
          </span>
        </button>
        {!isPoolSelectionEmpty(selection) && (
          <button
            type="button"
            onClick={() => setSelection(emptyPoolSelection())}
            className="shrink-0 rounded px-1.5 py-0.5 text-[11px] text-slate-400 ring-1 ring-white/10 transition-colors hover:bg-white/5 hover:text-white"
          >
            {t("randomPool.reset")}
          </button>
        )}
      </div>
      {showBracketFilter && (
        <div className="flex flex-col gap-1">
          <span className="text-xs text-slate-400">{t("aiOpponent.bracket")}</span>
          <BracketFilter selected={bracketFilter} onChange={setBracketFilter} />
        </div>
      )}
      {fellBack ? (
        <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
          {t("randomPool.emptyWarning", { total: baseSize })}
        </div>
      ) : (
        baseConflict && (
          <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
            {t("randomPool.nonePlayable", { selected: selectedCount, total: baseSize })}
          </div>
        )
      )}
      {open && (
        <div className="flex flex-col gap-1">
          <span className="text-[10px] text-slate-500">{t("randomPool.hint")}</span>
          {rows.map(({ section, memberIds, toggleIds, inCount, state }) => {
            const label = section.name
              ?? (section.kind === "precon"
                ? t("randomPool.precons")
                : t("myDecks.sectionUnfiled"));
            const isExpanded = expanded.has(section.key);
            return (
              <div key={section.key} className="flex flex-col">
                <div className="flex items-center gap-1.5">
                  <input
                    type="checkbox"
                    aria-label={label}
                    aria-checked={state === "mixed" ? "mixed" : state === "all"}
                    checked={state === "all"}
                    ref={(el) => {
                      if (el) el.indeterminate = state === "mixed";
                    }}
                    onChange={() =>
                      setSelection(setPoolGroupIncluded(selection, section.key, toggleIds, state !== "all"))
                    }
                    className="h-4 w-4 shrink-0 accent-indigo-400"
                  />
                  <button
                    type="button"
                    onClick={() => toggleExpanded(section.key)}
                    aria-expanded={isExpanded}
                    className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
                  >
                    <span className="truncate text-xs text-slate-300">{label}</span>
                    <span className="shrink-0 text-[11px] tabular-nums text-slate-500">
                      {t("randomPool.count", { selected: inCount, total: memberIds.length })}
                    </span>
                    <Chevron expanded={isExpanded} />
                  </button>
                </div>
                {isExpanded && (
                  <div className="ml-5 flex flex-col gap-0.5 border-l border-white/8 pl-2">
                    {section.members.map((member) => {
                      const checked = included.get(member.id) ?? true;
                      return (
                        <label
                          key={member.id}
                          className="flex cursor-pointer items-center gap-2 rounded px-1 py-0.5 transition-colors hover:bg-white/4"
                        >
                          <input
                            type="checkbox"
                            checked={checked}
                            onChange={() =>
                              setSelection(setPoolDeckIncluded(selection, member.id, !checked))
                            }
                            className="h-3.5 w-3.5 shrink-0 accent-indigo-400"
                          />
                          <span className="truncate text-xs text-slate-300">{member.name}</span>
                        </label>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

function Chevron({ expanded }: { expanded: boolean }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 20 20"
      className={`h-3.5 w-3.5 shrink-0 text-slate-500 transition-transform ${
        expanded ? "rotate-180" : ""
      }`}
      fill="currentColor"
    >
      <path
        fillRule="evenodd"
        d="M5.23 7.21a.75.75 0 0 1 1.06.02L10 11.06l3.71-3.83a.75.75 0 1 1 1.08 1.04l-4.25 4.39a.75.75 0 0 1-1.08 0L5.21 8.27a.75.75 0 0 1 .02-1.06Z"
        clipRule="evenodd"
      />
    </svg>
  );
}
