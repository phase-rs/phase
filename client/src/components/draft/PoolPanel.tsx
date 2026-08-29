import { useTranslation } from "react-i18next";
import type { ReactNode } from "react";

import { useDraftStore } from "../../stores/draftStore";
import type { PoolSortMode } from "../../stores/draftStore";
import type {
  DraftCardInstance,
  DraftPoolColorCounts,
  DraftPoolGroup,
  DraftPoolGroups,
  DraftPlayerView,
} from "../../adapter/draft-adapter";
import type { CardHoverInfo } from "../card/CardPreview";
import { POOL_GROUP_LABEL_KEYS } from "./poolGroupLabels";
import {
  activateWorkspaceInstance,
  resolveAvailableBoardSort,
} from "./workspace/workspacePlacement";
import type {
  DraftWorkspaceFilter,
  DraftWorkspaceState,
  DraftZone,
} from "./workspace/types";
import type { DraftBoardPreferences, DraftBoardSort } from "./workspace/workspacePreferences";
import { PopoverMenu } from "../menu/PopoverMenu";
import { menuButtonClass } from "../menu/buttonStyles";

const EMPTY_COLOR_COUNTS: DraftPoolColorCounts = {
  white: 0,
  blue: 0,
  black: 0,
  red: 0,
  green: 0,
};

const COLOR_COUNT_KEYS = {
  W: "white",
  U: "blue",
  B: "black",
  R: "red",
  G: "green",
} as const;

// ── Rarity badge ────────────────────────────────────────────────────────

const RARITY_DOT: Record<string, string> = {
  mythic: "bg-amber-400",
  rare: "bg-yellow-300",
  uncommon: "bg-slate-300",
  common: "bg-slate-500",
};

function rarityDotClass(rarity: string): string {
  return RARITY_DOT[rarity.toLowerCase()] ?? "bg-slate-500";
}

// ── Color pips ──────────────────────────────────────────────────────────

const COLOR_PIP: Record<string, string> = {
  W: "bg-amber-100",
  U: "bg-blue-400",
  B: "bg-purple-400",
  R: "bg-red-400",
  G: "bg-green-400",
};

function ColorPips({ colors }: { colors: string[] }) {
  if (colors.length === 0) {
    return <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-slate-500" />;
  }
  return (
    <span className="flex shrink-0 gap-0.5">
      {colors.map((c) => (
        <span
          key={c}
          className={`h-1.5 w-1.5 rounded-full ${COLOR_PIP[c] ?? "bg-slate-500"}`}
        />
      ))}
    </span>
  );
}

// ── Sort mode tabs ──────────────────────────────────────────────────────

const SORT_MODES: Array<{ mode: PoolSortMode; labelKey: string }> = [
  { mode: "color", labelKey: "pool.sortColor" },
  { mode: "type", labelKey: "pool.sortType" },
  { mode: "cmc", labelKey: "pool.sortCmc" },
];

// ── Component ───────────────────────────────────────────────────────────

export interface ControlledWorkspacePool {
  pool: readonly DraftCardInstance[];
  poolGroups: DraftPoolGroups;
  workspace: DraftWorkspaceState;
  preferences: Readonly<Record<DraftZone, DraftBoardPreferences>>;
  filter: DraftWorkspaceFilter;
  sort: DraftBoardSort;
  onFilterChange(filter: DraftWorkspaceFilter): void;
  onSortChange(sort: DraftBoardSort): void;
  onWorkspaceChange(next: DraftWorkspaceState): void;
  /** Primary control shown after sorting, such as Add lands. */
  compactPrimaryControls?: ReactNode;
  /** Optional deck counts shown immediately after the primary control. */
  compactCount?: ReactNode;
  /** Trailing control, such as the Visual builder switch. */
  compactTrailingControls?: ReactNode;
  /** Phone/tablet builder compact uses a single grouping menu. */
  builderCompact?: boolean;
}

interface PoolPanelProps {
  onCardHover?: (info: CardHoverInfo | null) => void;
  view?: DraftPlayerView | null;
  controlledWorkspace?: ControlledWorkspacePool;
}

function groupsForSort(sort: DraftBoardSort, groups: DraftPoolGroups): readonly DraftPoolGroup[] {
  switch (sort) {
    case "cmc": return groups.cmc_groups;
    case "color": return groups.color_groups;
    case "rarity": return groups.rarity_groups;
    case "type": return groups.type_groups;
  }
}

function ControlledPoolPanel({
  value,
  onCardHover,
}: {
  value: ControlledWorkspacePool;
  onCardHover?: (info: CardHoverInfo | null) => void;
}) {
  const { t } = useTranslation("draft");
  const effectiveSort = resolveAvailableBoardSort(
    value.sort,
    value.poolGroups.workspace_capabilities,
  );
  const liveCards = new Map(value.pool.map((card) => [card.instance_id, card]));
  const liveIds = new Set([
    ...liveCards.keys(),
    ...value.workspace.virtualBasics.map((basic) => basic.instanceId),
  ]);
  const countFor = (filter: DraftWorkspaceFilter) => [...liveIds].filter((instanceId) => (
    filter === "combined" || value.workspace.placements[instanceId]?.zone === filter
  )).length;
  const visibleIds = new Set([...liveIds].filter((instanceId) => (
    value.filter === "combined"
    || value.workspace.placements[instanceId]?.zone === value.filter
  )));
  const seenIds = new Set<string>();
  const renderedGroups = groupsForSort(effectiveSort, value.poolGroups).flatMap((group) => {
    const cards = group.cards.flatMap((entry) => entry.instance_ids.flatMap((instanceId) => {
      const card = liveCards.get(instanceId);
      if (card === undefined || !visibleIds.has(instanceId) || seenIds.has(instanceId)) return [];
      seenIds.add(instanceId);
      return [card];
    }));
    return cards.length === 0 ? [] : [{ key: group.kind, label: t(POOL_GROUP_LABEL_KEYS[group.kind]), cards }];
  });
  const supplemental = [...visibleIds].filter((instanceId) => !seenIds.has(instanceId));
  const sorts: DraftBoardSort[] = value.poolGroups.workspace_capabilities.rarity_group_order === null
    ? ["cmc", "color", "type"]
    : ["cmc", "color", "rarity", "type"];
  const sortButtonClass = (sort: DraftBoardSort, compactTouchTarget = false) => (
    `${compactTouchTarget ? "min-h-11" : "min-h-8"} px-2 text-xs ${effectiveSort === sort ? "bg-white/15 text-white" : "text-white/50"}`
  );

  const activate = (instanceId: string) => {
    const next = activateWorkspaceInstance(
      value.workspace,
      value.pool,
      value.poolGroups,
      value.preferences,
      instanceId,
    );
    if (next !== value.workspace) value.onWorkspaceChange(next);
  };

  const renderCard = (instanceId: string, card?: DraftCardInstance) => {
    const basic = value.workspace.virtualBasics.find((entry) => entry.instanceId === instanceId);
    const name = card?.name ?? basic?.name ?? instanceId;
    const sourcePrinting = card === undefined ? undefined : {
      setCode: card.set_code,
      collectorNumber: card.collector_number,
    };
    const zone = value.workspace.placements[instanceId]?.zone ?? "deck";
    const targetZone = zone === "deck" ? "sideboard" : "deck";
    const preview = { name, sourcePrinting };
    return (
      <div
        key={instanceId}
        data-instance-id={instanceId}
        className="flex min-h-9 items-center gap-2 border-b border-white/5 px-2 py-1 text-xs"
      >
        <button
          type="button"
          className="min-w-0 flex-1 truncate text-left text-white/80 focus-visible:outline-2 focus-visible:outline-amber-300"
          onMouseEnter={() => onCardHover?.(preview)}
          onMouseLeave={() => onCardHover?.(null)}
          onFocus={() => onCardHover?.(preview)}
          onBlur={() => onCardHover?.(null)}
          onClick={() => activate(instanceId)}
        >
          {name}
        </button>
        {card !== undefined && <ColorPips colors={card.colors} />}
        <button
          type="button"
          className="shrink-0 border border-white/20 px-2 py-1 text-white/70 hover:border-amber-300 hover:text-amber-200"
          aria-label={basic === undefined
            ? t("workspace.card.moveToZone", {
              card: name,
              zone: t(`workspace.zone.${targetZone}`),
            })
            : t("limitedDeck.removeCard", { name })}
          onClick={() => activate(instanceId)}
        >
          {basic === undefined ? t(`workspace.zone.${targetZone}`) : <span aria-hidden="true">×</span>}
        </button>
      </div>
    );
  };

  return (
    <section className="flex h-full min-h-0 flex-col overflow-hidden" aria-label={t("workspace.pool.label")}>
      <div role="group" aria-label={t("workspace.pool.filterLabel")} className="grid grid-cols-3 border-b border-white/10">
        {(["combined", "deck", "sideboard"] as const).map((filter) => (
          <button
            key={filter}
            type="button"
            aria-pressed={value.filter === filter}
            onClick={() => value.onFilterChange(filter)}
            className={`min-h-10 px-2 text-xs ${value.filter === filter ? "bg-amber-300 text-black" : "text-white/70"}`}
          >
            {t(`workspace.pool.filter.${filter}`, { count: countFor(filter) })}
          </button>
        ))}
      </div>
      <div role="group" aria-label={t("workspace.pool.sortLabel")} className="flex flex-wrap gap-1 border-b border-white/10 p-2">
        <div data-compact-pool-primary-controls className={`flex min-w-0 shrink-0 items-center gap-1 ${value.builderCompact ? "w-full flex-nowrap" : "flex-wrap"}`}>
          {value.builderCompact ? (
            <PopoverMenu
              ariaLabel={t("workspace.sort.group")}
              menuWidthPx={160}
              renderTrigger={({ ref, open, toggle }) => (
                <button
                  ref={ref}
                  type="button"
                  aria-expanded={open}
                  aria-haspopup="menu"
                  onClick={toggle}
                  className={menuButtonClass({ tone: "neutral", size: "xs", className: "min-h-11 shrink-0 whitespace-nowrap" })}
                >
                  {t("workspace.sort.group")}
                </button>
              )}
            >
              {(close) => sorts.map((sort) => (
                <button
                  key={sort}
                  type="button"
                  role="menuitemradio"
                  aria-checked={effectiveSort === sort}
                  aria-pressed={effectiveSort === sort}
                  onClick={() => {
                    value.onSortChange(sort);
                    close();
                  }}
                  className={sortButtonClass(sort, true)}
                >
                  {t(`workspace.sort.${sort}`)}
                </button>
              ))}
            </PopoverMenu>
          ) : sorts.map((sort) => (
            <button
              key={sort}
              type="button"
              aria-pressed={effectiveSort === sort}
              onClick={() => value.onSortChange(sort)}
              className={sortButtonClass(sort)}
            >
              {t(`workspace.sort.${sort}`)}
            </button>
          ))}
          {value.compactPrimaryControls}
          {value.compactCount}
          {value.compactTrailingControls && (
            <div data-compact-pool-trailing-controls className={value.builderCompact ? "ml-auto" : undefined}>
              {value.compactTrailingControls}
            </div>
          )}
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto p-2">
        {renderedGroups.map((group) => (
          <section key={group.key} className="mb-3">
            <h3 className="mb-1 text-xs font-semibold uppercase text-white/45">
              {group.label} ({group.cards.length})
            </h3>
            {group.cards.map((card) => renderCard(card.instance_id, card))}
          </section>
        ))}
        {supplemental.length > 0 && (
          <section>
            <h3 className="mb-1 text-xs font-semibold uppercase text-white/45">
              {t("workspace.headers.addedBasics")} ({supplemental.length})
            </h3>
            {supplemental.map((instanceId) => renderCard(instanceId, liveCards.get(instanceId)))}
          </section>
        )}
        {visibleIds.size === 0 && (
          <div className="py-4 text-center text-xs text-white/30">
            {t("workspace.pool.empty")}
          </div>
        )}
      </div>
    </section>
  );
}

export function PoolPanel({
  onCardHover,
  view: viewOverride,
  controlledWorkspace,
}: PoolPanelProps = {}) {
  const { t } = useTranslation("draft");
  const quickView = useDraftStore((s) => s.view);
  const poolSortMode = useDraftStore((s) => s.poolSortMode);
  const poolPanelOpen = useDraftStore((s) => s.poolPanelOpen);
  const setPoolSortMode = useDraftStore((s) => s.setPoolSortMode);
  const togglePoolPanel = useDraftStore((s) => s.togglePoolPanel);
  const view = viewOverride !== undefined ? viewOverride : quickView;

  const pool = view?.pool ?? [];
  const groups = view
    ? poolSortMode === "color"
      ? view.pool_groups.color_groups
      : poolSortMode === "type"
        ? view.pool_groups.type_groups
        : view.pool_groups.cmc_groups
    : [];
  const colorCounts = view?.pool_groups.color_counts ?? EMPTY_COLOR_COUNTS;

  if (controlledWorkspace !== undefined) {
    return <ControlledPoolPanel value={controlledWorkspace} onCardHover={onCardHover} />;
  }

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-white/10 px-3 py-2">
        <button
          onClick={togglePoolPanel}
          className="flex items-center gap-2 text-sm text-white/60 transition-colors hover:text-white"
        >
          <span className={`transition-transform ${poolPanelOpen ? "rotate-0" : "-rotate-90"}`}>
            ▼
          </span>
          <span className="font-medium">{t("pool.cardsDrafted", { count: pool.length })}</span>
        </button>
      </div>

      {!poolPanelOpen && null}

      {poolPanelOpen && (
        <>
          {/* CR 903.13e: the fillers this draft's sets let the player ADD to
              their card pool, each usable only as a commander. One line per
              grant — a mixed-set draft concedes one card per contained set, and
              each carries its own cap. Engine-derived and merely displayed —
              the caps and the commander-only condition are enforced at
              submission. */}
          {view?.grantable_commander_fillers?.map((granted) => (
            <div
              key={granted.card_name}
              className="border-b border-white/8 px-3 py-2 text-[11px] text-white/45"
            >
              {t("pool.grantedFiller", {
                name: granted.card_name,
                maximum: granted.max_copies,
              })}
            </div>
          ))}
          {/* WUBRG color-count strip (design mockup): how deep the pool is in
              each color. */}
          <div className="grid grid-cols-5 gap-1.5 border-b border-white/8 px-3 py-2">
            {(["W", "U", "B", "R", "G"] as const).map((c) => (
              <div key={c} className="flex flex-col items-center gap-1 rounded-[8px] bg-black/24 py-1.5">
                <span className={`h-3 w-3 rounded-full ${COLOR_PIP[c]} shadow-[inset_0_0_0_1px_rgba(0,0,0,0.3)]`} />
                <span className={`font-mono text-[11px] tabular-nums ${colorCounts[COLOR_COUNT_KEYS[c]] ? "text-slate-300" : "text-slate-600"}`}>
                  {colorCounts[COLOR_COUNT_KEYS[c]]}
                </span>
              </div>
            ))}
          </div>

          {/* Sort tabs */}
          <div className="flex gap-1 border-b border-white/8 px-3 py-2">
            {SORT_MODES.map(({ mode, labelKey }) => (
              <button
                key={mode}
                onClick={() => setPoolSortMode(mode)}
                className={`rounded-[12px] px-2.5 py-1 text-xs font-medium transition-colors ${
                  poolSortMode === mode
                    ? "bg-white/10 text-white"
                    : "text-white/40 hover:bg-white/5 hover:text-white/70"
                }`}
              >
                {t(labelKey)}
              </button>
            ))}
          </div>

          {/* Card groups */}
          <div className="flex-1 space-y-3 overflow-y-auto px-3 py-2">
            {groups.map((group) => (
              <div key={group.kind}>
                <div className="mb-1 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
                  {t(POOL_GROUP_LABEL_KEYS[group.kind])} ({group.total})
                </div>
                <div className="space-y-0.5">
                  {group.cards.map(({ card, count }) => (
                    <div
                      key={card.instance_id}
                      onMouseEnter={onCardHover ? () => onCardHover({ name: card.name, sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number } }) : undefined}
                      onMouseLeave={onCardHover ? () => onCardHover(null) : undefined}
                      className="flex items-center gap-2 rounded-[10px] px-2 py-1 text-xs transition-colors hover:bg-white/5"
                    >
                      <span className={`h-2 w-2 shrink-0 rounded-full ${rarityDotClass(card.rarity)}`} />
                      {count > 1 && (
                        <span className="flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full bg-white/10 px-1 text-[10px] font-medium text-white/60">
                          {count}
                        </span>
                      )}
                      <span className="truncate text-white/80">{card.name}</span>
                      <span className="ml-auto flex shrink-0 items-center gap-1.5">
                        <ColorPips colors={card.colors} />
                        <span className="text-white/30">{card.cmc}</span>
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            ))}

            {pool.length === 0 && (
              <div className="py-4 text-center text-xs text-white/30">
                {t("pool.empty")}
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}
