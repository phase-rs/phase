import { useTranslation } from "react-i18next";

import { useDraftStore } from "../../stores/draftStore";
import type { PoolSortMode } from "../../stores/draftStore";
import type { DraftPoolColorCounts, DraftPlayerView } from "../../adapter/draft-adapter";
import type { CardHoverInfo } from "../card/CardPreview";
import { POOL_GROUP_LABEL_KEYS } from "./poolGroupLabels";

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

interface PoolPanelProps {
  onCardHover?: (info: CardHoverInfo | null) => void;
  view?: DraftPlayerView | null;
}

export function PoolPanel({ onCardHover, view: viewOverride }: PoolPanelProps = {}) {
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
          {/* CR 903.13e: the filler this draft's set lets the player ADD to their
              card pool, usable only as a commander. Engine-derived and merely
              displayed — the cap and the commander-only condition are enforced
              at submission. */}
          {view?.grantable_commander_filler && (
            <div className="border-b border-white/8 px-3 py-2 text-[11px] text-white/45">
              {t("pool.grantedFiller", {
                name: view.grantable_commander_filler.card_name,
                maximum: view.grantable_commander_filler.max_copies,
              })}
            </div>
          )}
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
