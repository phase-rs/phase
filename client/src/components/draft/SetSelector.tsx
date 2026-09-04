import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { useTranslation } from "react-i18next";

import type { DraftPackChoice } from "../../stores/draftStore";
import { menuButtonClass } from "../menu/buttonStyles";
import { BotDifficultySelector } from "./BotDifficultySelector";

// ── Types ───────────────────────────────────────────────────────────────

interface SetPoolEntry {
  name?: string;
  code?: string;
  [key: string]: unknown;
}

interface ScryfallSetEntry {
  name: string;
  icon_svg_uri: string;
  released_at: string;
}

interface AvailableSet {
  code: string;
  name: string;
  icon?: string;
  releasedAt: string;
}

interface SetSelectorProps {
  /**
   * Boosters the player arranged, in pack order. Duplicates are expected —
   * the same set may fill several packs.
   */
  onStartDraft: (packs: DraftPackChoice[]) => void;
  /**
   * Packs the event opens by default. Sealed is fixed at six by the engine, so
   * the pack list is locked to that length; a draft may open however many the
   * player lines up, and this is the count the "fill the remaining packs"
   * shortcut tops the list up to.
   */
  defaultPackCount: number;
  /** Sealed events must name exactly `defaultPackCount` boosters. */
  fixedPackCount?: boolean;
  /**
   * Select a distinct candidate pool rather than an ordered booster lineup.
   * Chaos assignment is resolved by the host engine; this component only
   * collects the candidate set intent.
   */
  candidatePool?: boolean;
  /**
   * Text on the start button. Defaults to "Start Draft"; Sealed passes its own
   * word, since the same selector runs both events.
   */
  startLabel?: string;
  /**
   * Draft every pack from one set, chosen with a single click and no pack list.
   *
   * No in-tree caller takes this path any more — pods carry a per-pack sequence
   * like every other set-backed event since multi-set drafts landed. Kept for a
   * surface that wants a one-click set picker; the same result is reachable from
   * the normal path by naming one set, which then fills every booster.
   */
  singleSet?: boolean;
}

/**
 * Hold the content below a resizing block still while the block changes height.
 *
 * Adding, removing, or clearing a pack changes the pack list's height, and the
 * set grid sits below it. Without this, every click slides the grid — including
 * the tile the pointer is on — by the height the list just gained, which reads
 * as the page jumping. The browser's own scroll anchoring does not cover it:
 * React replaces the nodes it would anchor to.
 *
 * Only growth above the fold is compensated. A list the player can see growing
 * downward is expected, and scrolling then would drag them off the top.
 */
function useStableScrollBelow(
  ref: RefObject<HTMLElement | null>,
  changeKey: number,
): void {
  const measuredHeight = useRef<number | null>(null);

  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;

    const { height, top } = element.getBoundingClientRect();
    const previous = measuredHeight.current;
    measuredHeight.current = height;

    if (previous === null || previous === height || top >= 0) return;
    window.scrollBy({ top: height - previous, behavior: "instant" });
  }, [ref, changeKey]);
}

// ── Component ───────────────────────────────────────────────────────────

export function SetSelector({
  onStartDraft,
  defaultPackCount,
  fixedPackCount = false,
  candidatePool = false,
  startLabel,
  singleSet = false,
}: SetSelectorProps) {
  const { t } = useTranslation("draft");

  const [sets, setSets] = useState<AvailableSet[]>([]);
  const [loading, setLoading] = useState(true);
  // `null` = no error. `{ detail }` carries a technical message; `detail`
  // undefined means "use the generic translated fallback". Translation happens
  // at render so the load effect never closes over `t` (avoids re-fetch on
  // language change).
  const [error, setError] = useState<{ detail?: string } | null>(null);
  /** The pack list, in pack order. Index 0 is pack 1. */
  const [packs, setPacks] = useState<DraftPackChoice[]>([]);

  useEffect(() => {
    let cancelled = false;

    async function loadSets() {
      try {
        const [poolsResp, setsResp] = await Promise.all([
          fetch(__DRAFT_POOLS_URL__),
          fetch(__SCRYFALL_SETS_URL__),
        ]);
        if (!poolsResp.ok) throw new Error(`Failed to load draft pools: ${poolsResp.status}`);

        const pools: Record<string, SetPoolEntry> = await poolsResp.json();
        const scryfallSets: Record<string, ScryfallSetEntry> = setsResp.ok
          ? await setsResp.json()
          : {};

        if (cancelled) return;

        const entries = Object.entries(pools).map(([code, entry]) => ({
          code: code.toUpperCase(),
          name: (entry.name as string) ?? code.toUpperCase(),
          icon: scryfallSets[code]?.icon_svg_uri,
          releasedAt: scryfallSets[code]?.released_at ?? "",
        }));

        entries.sort((a, b) => b.releasedAt.localeCompare(a.releasedAt));
        setSets(entries);
      } catch (err) {
        if (!cancelled) {
          setError({ detail: err instanceof Error ? err.message : undefined });
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    }

    loadSets();
    return () => { cancelled = true; };
  }, []);

  // An event may not carry a list from a previous mode that is longer than it
  // can open. This also applies to flexible drafts: a short list repeats its
  // last set, but a longer list is rejected by the selection authority.
  useEffect(() => {
    setPacks((current) => {
      if (!candidatePool) return current.slice(0, defaultPackCount);
      return current.filter(
        (pack, index) => current.findIndex((candidate) => candidate.code === pack.code) === index,
      );
    });
  }, [candidatePool, defaultPackCount]);

  // The pack list grows and shrinks above the set grid; keep the grid still.
  const packListRef = useRef<HTMLDivElement | null>(null);
  useStableScrollBelow(packListRef, packs.length);

  // The event's own booster count is the ceiling, fixed-length or not. Naming
  // MORE sets than the event opens is refused by the engine
  // (`ResolvedSetSelection`), so offering a longer list would only build a
  // selection that cannot start. Naming FEWER is fine: a short sequence repeats
  // its last entry, which is how one click still fills every pack.
  const packLimit = defaultPackCount;
  const isFull = !singleSet && !candidatePool && packs.length >= packLimit;
  const canStart = !candidatePool && fixedPackCount
    ? packs.length === defaultPackCount
    : packs.length > 0;

  const appendPack = useCallback(
    (set: AvailableSet) => {
      setPacks((current) =>
        (!candidatePool && current.length >= packLimit)
          || (candidatePool && current.some((pack) => pack.code === set.code))
          ? current
          : [...current, { code: set.code, name: set.name }],
      );
    },
    [candidatePool, packLimit],
  );

  /**
   * Repeat the last chosen set until the event's default pack count is met —
   * the short path to a single-set draft, and to topping up a mixed one.
   */
  const fillRemaining = useCallback(() => {
    setPacks((current) => {
      const last = current[current.length - 1];
      if (!last) return current;
      const remaining = Math.max(0, defaultPackCount - current.length);
      return [...current, ...Array.from({ length: remaining }, () => ({ ...last }))];
    });
  }, [defaultPackCount]);

  const removePack = useCallback((index: number) => {
    setPacks((current) => current.filter((_, i) => i !== index));
  }, []);

  /** Swap a pack with its neighbour, overriding the pick order it defaulted to. */
  const movePack = useCallback((index: number, delta: -1 | 1) => {
    setPacks((current) => {
      const target = index + delta;
      if (target < 0 || target >= current.length) return current;
      const next = [...current];
      [next[index], next[target]] = [next[target], next[index]];
      return next;
    });
  }, []);

  return (
    <div className="flex flex-col gap-6">
      <BotDifficultySelector />

      {!singleSet && (
        <PackList
          containerRef={packListRef}
          packs={packs}
          packLimit={packLimit}
          fixedPackCount={fixedPackCount}
          candidatePool={candidatePool}
          defaultPackCount={defaultPackCount}
          canStart={canStart}
          startLabel={startLabel ?? t("setSelector.startDraft")}
          onStart={() => onStartDraft(packs)}
          onMove={movePack}
          onRemove={removePack}
          onClear={() => setPacks([])}
          onFillRemaining={fillRemaining}
        />
      )}

      {/* Set grid */}
      <div className="flex flex-col gap-2">
        <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
          {singleSet
          ? t("setSelector.chooseSet")
            : isFull
              ? t("setSelector.packsFull")
              : candidatePool
                ? t("setSelector.addCandidateSet")
                : t("setSelector.addPackFromSet")}
        </h3>

        {error && (
          <div className="py-4 text-center text-sm text-red-300">
            {error.detail ?? t("setSelector.loadFailed")}
          </div>
        )}

        {!loading && !error && sets.length === 0 && (
          <div className="py-8 text-center text-sm text-white/40">
            {t("setSelector.noPools")}
          </div>
        )}

        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
          {loading
            ? Array.from({ length: 10 }, (_, i) => (
                <div
                  key={i}
                  className="flex animate-pulse flex-col items-center gap-2 rounded-[16px] border border-white/8 bg-black/18 p-4"
                >
                  <div className="h-10 w-10 rounded-full bg-white/10" />
                  <div className="h-2.5 w-3/4 rounded bg-white/8" />
                </div>
              ))
            : sets.map((set) => {
                const packCount = packs.filter((pack) => pack.code === set.code).length;
                return (
                  <SetTile
                    key={set.code}
                    set={set}
                    packCount={singleSet ? 0 : packCount}
                    disabled={isFull}
                    label={singleSet
                      ? t("setSelector.draftSet", { name: set.name })
                      : candidatePool
                        ? t("setSelector.addCandidate", { name: set.name })
                        : t("setSelector.addPackOf", { name: set.name })}
                    onAdd={() =>
                      singleSet
                        ? onStartDraft([{ code: set.code, name: set.name }])
                        : appendPack(set)
                    }
                  />
                );
              })}
        </div>
      </div>
    </div>
  );
}

// ── Pack list ───────────────────────────────────────────────────────────

function PackList({
  containerRef,
  packs,
  packLimit,
  fixedPackCount,
  candidatePool,
  defaultPackCount,
  canStart,
  startLabel,
  onStart,
  onMove,
  onRemove,
  onClear,
  onFillRemaining,
}: {
  containerRef: RefObject<HTMLDivElement | null>;
  packs: DraftPackChoice[];
  packLimit: number;
  fixedPackCount: boolean;
  candidatePool: boolean;
  defaultPackCount: number;
  canStart: boolean;
  startLabel: string;
  onStart: () => void;
  onMove: (index: number, delta: -1 | 1) => void;
  onRemove: (index: number) => void;
  onClear: () => void;
  onFillRemaining: () => void;
}) {
  const { t } = useTranslation("draft");

  const lastPack = packs[packs.length - 1];
  const remaining = defaultPackCount - packs.length;

  return (
    <div ref={containerRef} className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
          {candidatePool ? t("setSelector.candidateSets") : t("setSelector.packOrder")}
        </h3>
        <div className="flex items-center gap-2">
          {packs.length > 0 && (
            <button
              type="button"
              onClick={onClear}
              className={menuButtonClass({ tone: "neutral", size: "xs", ghost: true })}
            >
              {t("setSelector.clearPacks")}
            </button>
          )}
          <button
            type="button"
            disabled={!canStart}
            onClick={onStart}
            className={menuButtonClass({ tone: "emerald", size: "sm", disabled: !canStart })}
          >
            {startLabel}
          </button>
        </div>
      </div>

      {packs.length === 0 ? (
        <p className="rounded-[16px] border border-dashed border-white/10 bg-black/12 px-4 py-5 text-sm text-white/40">
          {candidatePool
            ? t("setSelector.emptyCandidates")
            : fixedPackCount
            ? t("setSelector.emptyPacksFixed", { count: defaultPackCount })
            : t("setSelector.emptyPacks", { count: defaultPackCount })}
        </p>
      ) : (
        <ol className="flex flex-col gap-1.5">
          {packs.map((pack, index) => (
            <li
              key={`${pack.code}-${index}`}
              className="flex items-center gap-3 rounded-[12px] border border-white/10 bg-black/18 px-3 py-2 backdrop-blur-md"
            >
              <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full border border-white/15 bg-white/8 text-[11px] font-semibold tabular-nums text-white/70">
                {index + 1}
              </span>
              <span className="min-w-0 flex-1 truncate text-sm text-white/80">
                {pack.name}
              </span>
              <span className="shrink-0 text-[11px] font-semibold tracking-wider text-white/35">
                {pack.code}
              </span>
              <div className="flex shrink-0 items-center gap-1">
                {!candidatePool && (
                  <>
                    <button
                      type="button"
                      onClick={() => onMove(index, -1)}
                      disabled={index === 0}
                      aria-label={t("setSelector.movePackEarlier", { number: index + 1 })}
                      className={menuButtonClass({
                        tone: "neutral",
                        size: "icon",
                        ghost: true,
                        disabled: index === 0,
                      })}
                    >
                      ↑
                    </button>
                    <button
                      type="button"
                      onClick={() => onMove(index, 1)}
                      disabled={index === packs.length - 1}
                      aria-label={t("setSelector.movePackLater", { number: index + 1 })}
                      className={menuButtonClass({
                        tone: "neutral",
                        size: "icon",
                        ghost: true,
                        disabled: index === packs.length - 1,
                      })}
                    >
                      ↓
                    </button>
                  </>
                )}
                <button
                  type="button"
                  onClick={() => onRemove(index)}
                  aria-label={t("setSelector.removePack", { number: index + 1 })}
                  className={menuButtonClass({ tone: "red", size: "icon", ghost: true })}
                >
                  ✕
                </button>
              </div>
            </li>
          ))}
        </ol>
      )}

      {!candidatePool && lastPack && remaining > 0 && (
        <button
          type="button"
          onClick={onFillRemaining}
          className={menuButtonClass({
            tone: "emerald",
            size: "xs",
            ghost: true,
            className: "self-start",
          })}
        >
          {t("setSelector.fillRemaining", {
            count: remaining,
            name: lastPack.name,
          })}
        </button>
      )}

      <p className="text-xs text-white/35">
        {candidatePool
          ? t("setSelector.candidateCount", { selected: packs.length })
          : fixedPackCount
          ? t("setSelector.packCountFixed", {
              selected: packs.length,
              required: defaultPackCount,
            })
          : t("setSelector.packCountFlexible", {
              selected: packs.length,
              max: packLimit,
            })}
      </p>
    </div>
  );
}

// ── Set tile ────────────────────────────────────────────────────────────

function SetTile({
  set,
  packCount,
  disabled,
  label,
  onAdd,
}: {
  set: AvailableSet;
  packCount: number;
  disabled: boolean;
  label: string;
  onAdd: () => void;
}) {
  const { t } = useTranslation("draft");

  return (
    <div className="relative">
      <button
        type="button"
        onClick={onAdd}
        disabled={disabled}
        aria-label={label}
        className={`flex w-full flex-col items-center gap-2 rounded-[16px] border p-4 backdrop-blur-md transition-colors ${
          disabled
            ? "cursor-not-allowed border-white/6 bg-black/10 opacity-45"
            : "cursor-pointer border-white/10 bg-black/18 hover:border-white/20 hover:bg-white/8"
        } ${packCount > 0 ? "border-emerald-300/35 bg-emerald-400/8" : ""}`}
      >
        {set.icon ? (
          <img
            src={set.icon}
            alt={t("setSelector.setIconAlt", { name: set.name })}
            className="h-10 w-10 invert opacity-80"
          />
        ) : (
          <span className="text-2xl font-bold tracking-wider text-white">
            {set.code}
          </span>
        )}
        <span className="text-center text-xs leading-tight text-white/55">
          {set.name}
        </span>
      </button>

      {packCount > 0 && (
        <span
          aria-hidden="true"
          className="pointer-events-none absolute right-2 top-2 flex h-5 min-w-5 items-center justify-center rounded-full bg-emerald-400/90 px-1.5 text-[11px] font-bold tabular-nums text-gray-950"
        >
          ×{packCount}
        </span>
      )}
    </div>
  );
}
