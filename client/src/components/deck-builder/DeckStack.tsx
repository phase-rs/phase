import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { useCardImage } from "../../hooks/useCardImage";
import { usePrintingsLoaded } from "../../hooks/usePrintingsLoaded";
import { hasAlternatePrintingsSync, resolveOracleIdSync } from "../../services/scryfall";
import type { DeckEntry, ParsedDeck } from "../../services/deckParser";
import type { SourcePrinting } from "../../hooks/useCardImage";
import type { ScryfallCard } from "../../services/scryfall";
import { usePreferencesStore } from "../../stores/preferencesStore";
import type { GameFormat } from "../../adapter/types";
import { DeckCardContextMenu } from "./DeckCardContextMenu";
import { PrintingPickerModal } from "./PrintingPickerModal";
import { mouseHoverPreview } from "./hoverPreview";
import { groupAccent, groupKey, groupRank, groupTitleKey, type GroupMode } from "./deckGrouping";
import { isMaybeboardPolicy, useSideboardPolicy } from "./useSideboardPolicy";

interface DeckStackProps {
  deck: ParsedDeck;
  commanders: string[];
  cardDataCache: Map<string, ScryfallCard>;
  onAddCard: (name: string) => void;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onMoveCard: (name: string, from: "main" | "sideboard") => void;
  onRemoveCommander: (cardName: string) => void;
  onCardHover?: (cardName: string | null, scryfallId?: string) => void;
  /** Deck format — resolves the sideboard policy so the second section
   *  is labelled "Sideboard" or "Maybeboard" consistently with the list view. */
  format?: GameFormat;
  /** Whether the main deck is sub-grouped by card type or by color. */
  groupMode: GroupMode;
}

type DeckStackSection = "commander" | "main" | "sideboard";

interface DeckStackItem {
  count: number;
  name: string;
  section: DeckStackSection;
  groupTitle: string;
  sortKey: [number, number, string];
  sourcePrinting?: SourcePrinting;
}

interface DeckStackGroup {
  key: string;
  title: string;
  entries: DeckStackItem[];
}

const CARD_HEIGHT = 156;
const CARD_WIDTH = 112;

function sortDeckStackItems(items: DeckStackItem[]): DeckStackItem[] {
  const next = [...items];
  next.sort((left, right) => {
    const [leftRank, leftCmc, leftName] = left.sortKey;
    const [rightRank, rightCmc, rightName] = right.sortKey;
    if (leftRank !== rightRank) return leftRank - rightRank;
    if (leftCmc !== rightCmc) return leftCmc - rightCmc;
    return leftName.localeCompare(rightName);
  });
  return next;
}

function createDeckStackItems(
  deck: ParsedDeck,
  commanders: string[],
  cardDataCache: Map<string, ScryfallCard>,
  mode: GroupMode,
): Record<DeckStackSection, DeckStackItem[]> {
  const commandersItems: DeckStackItem[] = [];
  for (const name of commanders) {
    const card = cardDataCache.get(name);
    commandersItems.push({
      count: 1,
      name,
      section: "commander",
      groupTitle: "",
      sortKey: [0, card?.cmc ?? 0, name.toLowerCase()],
    });
  }

  const mainItems: DeckStackItem[] = [];
  for (const entry of deck.main) {
    const card = cardDataCache.get(entry.name);
    mainItems.push({
      count: entry.count,
      name: entry.name,
      sourcePrinting: entry.sourcePrinting,
      section: "main",
      groupTitle: groupTitleKey(mode, groupKey(mode, card)),
      sortKey: [groupRank(mode, card), card?.cmc ?? 0, entry.name.toLowerCase()],
    });
  }

  const sideboardItems: DeckStackItem[] = [];
  for (const entry of deck.sideboard) {
    const card = cardDataCache.get(entry.name);
    sideboardItems.push({
      count: entry.count,
      name: entry.name,
      sourcePrinting: entry.sourcePrinting,
      section: "sideboard",
      groupTitle: groupTitleKey(mode, groupKey(mode, card)),
      sortKey: [groupRank(mode, card), card?.cmc ?? 0, entry.name.toLowerCase()],
    });
  }

  return {
    commander: sortDeckStackItems(commandersItems),
    main: sortDeckStackItems(mainItems),
    sideboard: sortDeckStackItems(sideboardItems),
  };
}

function totalCards(entries: DeckEntry[]): number {
  return entries.reduce((sum, entry) => sum + entry.count, 0);
}

function buildGroups(entries: DeckStackItem[]): DeckStackGroup[] {
  if (entries.length === 0) return [];

  const groups: DeckStackGroup[] = [];
  let currentTitle: string | null = null;
  let currentEntries: DeckStackItem[] = [];

  const flush = () => {
    if (currentTitle === null || currentEntries.length === 0) return;
    groups.push({
      key: `group-${currentTitle}`,
      title: currentTitle,
      entries: currentEntries,
    });
  };

  for (const entry of entries) {
    if (currentTitle !== entry.groupTitle) {
      flush();
      currentTitle = entry.groupTitle;
      currentEntries = [entry];
      continue;
    }
    currentEntries.push(entry);
  }

  flush();
  return groups;
}

function DeckStackCard({
  item,
  zIndex,
  className,
  canAdd,
  isMaybeboard,
  onAddCard,
  onRemoveCard,
  onMoveCard,
  onRemoveCommander,
  onCardHover,
  onContextMenu,
}: {
  item: DeckStackItem;
  zIndex: number;
  className?: string;
  canAdd: boolean;
  isMaybeboard: boolean;
  onAddCard: (name: string) => void;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onMoveCard: (name: string, from: "main" | "sideboard") => void;
  onRemoveCommander: (cardName: string) => void;
  onCardHover?: (cardName: string | null, scryfallId?: string) => void;
  onContextMenu?: (cardName: string, x: number, y: number) => void;
}) {
  const { t } = useTranslation("deck-builder");
  const { src, isLoading } = useCardImage(item.name, { size: "normal", sourcePrinting: item.sourcePrinting });
  const printingsLoaded = usePrintingsLoaded();
  const oracleId = printingsLoaded ? resolveOracleIdSync(item.name) : null;
  const hasAlternates = oracleId ? hasAlternatePrintingsSync(oracleId) : false;
  const isCommander = item.section === "commander";
  const showAddButton = item.section === "main";
  // The commander isn't part of the main/maybeboard partition, so it has no
  // move target. Main cards move out to the sideboard/maybeboard; second-section
  // cards move back to main — the destination is shown on the button label so
  // it's explicit on touch (where the title tooltip is invisible).
  const showMove = item.section !== "commander";
  const moveTargetLabel =
    item.section === "main"
      ? isMaybeboard
        ? t("stack.maybeboardName")
        : t("stack.sideboardName")
      : t("stack.mainName");

  // Buttons stop propagation so tapping +/-/move doesn't also fire the
  // card-body tap-to-preview (the controls sit on top of the card).
  const handleRemove = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (item.section === "commander") {
      onRemoveCommander(item.name);
      return;
    }
    onRemoveCard(item.name, item.section);
  };

  const handleAdd = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!canAdd) return;
    onAddCard(item.name);
  };

  const handleMove = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (item.section === "commander") return;
    onMoveCard(item.name, item.section);
  };

  return (
    <div
      className={`relative ${onCardHover ? "cursor-pointer" : ""} ${className ?? ""}`}
      style={{ zIndex, width: CARD_WIDTH }}
      // Tap previews the card on touch; hover previews on mouse (guarded so the
      // touch-compat mouseleave can't tear down the overlay the tap just opened).
      onClick={() => onCardHover?.(item.name)}
      {...mouseHoverPreview(onCardHover, item.name)}
      onContextMenu={(e) => {
        if (onContextMenu) {
          e.preventDefault();
          onContextMenu(item.name, e.clientX, e.clientY);
        }
      }}
    >
      <div
        className={`group relative overflow-hidden rounded-xl bg-black/35 shadow-[0_16px_36px_rgba(0,0,0,0.32)] ${
          isCommander
            ? "border-2 border-fuchsia-300/80 ring-2 ring-fuchsia-500/40"
            : "border border-white/12"
        }`}
      >
        <div className="absolute left-2 top-2 z-10 flex items-center gap-1">
          <span className="rounded-full bg-black/80 px-2 py-0.5 text-[10px] font-semibold text-white">
            {item.count}x
          </span>
          {isCommander && (
            <span className="rounded-full bg-fuchsia-200/95 px-2 py-0.5 text-[10px] font-bold text-fuchsia-950">
              {t("stack.commanderBadge")}
            </span>
          )}
          {hasAlternates && (
            <span
              className="rounded-full bg-sky-500/70 px-1.5 py-0.5 text-[10px] text-sky-50"
              title={t("card.alternateArtRightClick")}
            >
              ✦
            </span>
          )}
        </div>
        {/* Quantity controls are always visible (not hover-gated) so they're
            usable on touch, where :hover never fires. */}
        {showAddButton && (
          <button
            onClick={handleAdd}
            disabled={!canAdd}
            className="absolute right-10 top-2 z-10 flex h-6 w-6 items-center justify-center rounded-full bg-black/78 text-sm font-bold text-emerald-300 transition hover:bg-emerald-500/85 hover:text-white disabled:cursor-not-allowed disabled:text-slate-500 disabled:hover:bg-black/78"
            title={canAdd ? t("stack.addOne", { name: item.name }) : t("stack.copyLimit", { name: item.name })}
          >
            +
          </button>
        )}
        <button
          onClick={handleRemove}
          className="absolute right-2 top-2 z-10 flex h-6 w-6 items-center justify-center rounded-full bg-black/78 text-sm font-bold text-red-300 transition hover:bg-red-500/85 hover:text-white"
          title={
            item.section === "commander"
              ? t("stack.removeCommander", { name: item.name })
              : t("stack.removeOne", { name: item.name })
          }
        >
          -
        </button>
        {isLoading || !src ? (
          <div
            className="animate-pulse bg-slate-800"
            style={{ height: CARD_HEIGHT, width: CARD_WIDTH }}
          />
        ) : (
          <img
            src={src}
            alt={item.name}
            draggable={false}
            className="object-cover"
            style={{ height: CARD_HEIGHT, width: CARD_WIDTH }}
          />
        )}
        {/* The overlay is pointer-events-none so the card body stays tappable
            for preview; the move pill re-enables pointer events for itself. */}
        <div className="pointer-events-none absolute inset-x-0 bottom-0 flex items-end gap-1 bg-gradient-to-t from-black via-black/70 to-transparent px-2 pb-2 pt-8">
          <div className="min-w-0 flex-1 truncate text-[11px] font-medium text-white">
            {item.name}
          </div>
          {showMove && (
            <button
              type="button"
              onClick={handleMove}
              className="pointer-events-auto inline-flex shrink-0 items-center gap-0.5 whitespace-nowrap rounded-full bg-black/78 px-2 py-1 text-[10px] font-semibold text-sky-300 transition hover:bg-sky-500/85 hover:text-white"
              aria-label={t("card.moveToTarget", { name: item.name, target: moveTargetLabel })}
              title={t("card.moveToTarget", { name: item.name, target: moveTargetLabel })}
            >
              <span aria-hidden="true">→</span>
              {moveTargetLabel}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

function DeckStackSectionLane({
  title,
  badge,
  entries,
  emptyLabel,
  showGroupSections = false,
  extraGroups,
  isMaybeboard,
  onAddCard,
  canAddCard,
  onRemoveCard,
  onMoveCard,
  onRemoveCommander,
  onCardHover,
  onContextMenu,
}: {
  title: string;
  badge: string;
  entries: DeckStackItem[];
  emptyLabel: string;
  showGroupSections?: boolean;
  /** Extra groups rendered after the main groups (e.g. Sideboard appended
   *  to the Main Deck lane below the Lands subsection). */
  extraGroups?: DeckStackGroup[];
  isMaybeboard: boolean;
  onAddCard: (name: string) => void;
  canAddCard: (item: DeckStackItem) => boolean;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onMoveCard: (name: string, from: "main" | "sideboard") => void;
  onRemoveCommander: (cardName: string) => void;
  onCardHover?: (cardName: string | null, scryfallId?: string) => void;
  onContextMenu?: (cardName: string, x: number, y: number) => void;
}) {
  const { t } = useTranslation("deck-builder");
  const groups = useMemo(() => {
    const base = showGroupSections
      ? buildGroups(entries)
      : [{ key: "all", title: "", entries }];
    return extraGroups && extraGroups.length > 0 ? [...base, ...extraGroups] : base;
  }, [entries, showGroupSections, extraGroups]);
  const showGroupHeaders = showGroupSections || (extraGroups?.length ?? 0) > 0;

  return (
    <section className="flex min-w-0 flex-col rounded-[20px] border border-white/8 bg-black/14 px-3 py-3">
      <div className="mb-3 flex items-center justify-between">
        <div className="text-sm font-semibold text-white">{title}</div>
        <span className="rounded-full border border-white/10 bg-black/20 px-2 py-1 text-[11px] text-slate-300">
          {badge}
        </span>
      </div>

      {entries.length === 0 ? (
        <div className="flex min-h-[180px] items-center justify-center rounded-[16px] border border-dashed border-white/10 bg-black/10 text-sm text-slate-500">
          {emptyLabel}
        </div>
      ) : (
        <div className="pb-1">
          <div className="flex min-w-0 flex-col gap-6">
            {groups.map((group, groupIndex) => (
              <div key={group.key} className={groupIndex > 0 ? "pt-1" : undefined}>
                {showGroupHeaders && (() => {
                  const accent = groupAccent(group.title);
                  return (
                  <div className="mb-3 flex items-center gap-3">
                    <span className={`h-3.5 w-1 shrink-0 rounded-full ${accent.bar}`} aria-hidden="true" />
                    <div className={`text-[0.68rem] font-semibold uppercase tracking-[0.22em] ${accent.text}`}>
                      {t(`stack.${group.title}`)}
                    </div>
                    <div className="h-px flex-1 bg-white/8" />
                    <div className="text-[11px] text-slate-500">
                      {t("stack.groupCards", {
                        count: group.entries.reduce<number>((sum, entry) => sum + entry.count, 0),
                      })}
                    </div>
                  </div>
                  );
                })()}
                <div
                  className="grid justify-start gap-4"
                  style={{ gridTemplateColumns: `repeat(auto-fill, minmax(${CARD_WIDTH}px, ${CARD_WIDTH}px))` }}
                >
                  {group.entries.map((item, itemIndex) => (
                    <DeckStackCard
                      key={`${item.section}:${item.name}`}
                      item={item}
                      zIndex={group.entries.length - itemIndex}
                      canAdd={canAddCard(item)}
                      isMaybeboard={isMaybeboard}
                      onAddCard={onAddCard}
                      onRemoveCard={onRemoveCard}
                      onMoveCard={onMoveCard}
                      onRemoveCommander={onRemoveCommander}
                      onCardHover={onCardHover}
                      onContextMenu={onContextMenu}
                    />
                  ))}
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </section>
  );
}

export function DeckStack({
  deck,
  commanders,
  cardDataCache,
  onAddCard,
  onRemoveCard,
  onMoveCard,
  onRemoveCommander,
  onCardHover,
  format,
  groupMode,
}: DeckStackProps) {
  const { t } = useTranslation("deck-builder");
  const isMaybeboard = isMaybeboardPolicy(useSideboardPolicy(format));
  const sections = useMemo(
    () => createDeckStackItems(deck, commanders, cardDataCache, groupMode),
    [deck, commanders, cardDataCache, groupMode],
  );
  const mainDeckCount = totalCards(deck.main) + commanders.length;
  const sideboardCount = totalCards(deck.sideboard);
  const hasCards =
    sections.commander.length > 0
    || sections.main.length > 0
    || sections.sideboard.length > 0;
  const canAddCard = useMemo(
    () => (item: DeckStackItem) => item.section === "main",
    [],
  );

  const artOverrides = usePreferencesStore((s) => s.artOverrides);
  const clearArtOverride = usePreferencesStore((s) => s.clearArtOverride);

  // The second section renders as a subsection appended to the Main Deck lane
  // below the Lands subsection, so it shows up naturally as you scroll the
  // visual stack. Titled "Maybeboard" for Forbidden-policy formats, else
  // "Sideboard" — consistent with the list view.
  const sideboardGroups = useMemo<DeckStackGroup[]>(
    () =>
      sections.sideboard.length > 0
        ? [
            {
              key: "sideboard",
              title: isMaybeboard ? "maybeboardGroup" : "sideboardGroup",
              entries: sections.sideboard,
            },
          ]
        : [],
    [sections.sideboard, isMaybeboard],
  );

  const [contextMenu, setContextMenu] = useState<{ cardName: string; x: number; y: number } | null>(null);
  const [pickerCard, setPickerCard] = useState<{ cardName: string; oracleId: string } | null>(null);

  const handleContextMenu = useCallback((cardName: string, x: number, y: number) => {
    setContextMenu({ cardName, x, y });
  }, []);

  const handleChooseArt = useCallback(() => {
    if (!contextMenu) return;
    const oracleId = resolveOracleIdSync(contextMenu.cardName);
    if (oracleId) {
      setPickerCard({ cardName: contextMenu.cardName, oracleId });
    }
  }, [contextMenu]);

  const handleClearOverride = useCallback(() => {
    if (!contextMenu) return;
    const oracleId = resolveOracleIdSync(contextMenu.cardName);
    if (oracleId) clearArtOverride(oracleId);
  }, [contextMenu, clearArtOverride]);

  return (
    <div className="flex h-full min-h-0 flex-col overflow-hidden">
      <div className="flex items-center justify-between border-b border-white/8 px-3 py-2">
        <div>
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">{t("stack.deckView")}</div>
          <div className="mt-1 text-sm font-semibold text-white">{t("stack.visualDeckStack")}</div>
        </div>
        <div className="flex items-center gap-2 text-[11px] text-slate-300">
          <span className="rounded-full border border-white/10 bg-black/20 px-2 py-1">
            {t("stack.mainBadge", { count: mainDeckCount })}
          </span>
          {sideboardCount > 0 && (
            <span className="rounded-full border border-white/10 bg-black/20 px-2 py-1">
              {isMaybeboard
                ? t("stack.maybeboardBadge", { count: sideboardCount })
                : t("stack.sideboardBadge", { count: sideboardCount })}
            </span>
          )}
        </div>
      </div>

      <div className="thin-scrollbar flex-1 overflow-auto px-3 pt-4 pb-16">
        {!hasCards ? (
          <div className="flex h-full items-center justify-center rounded-[20px] border border-dashed border-white/10 bg-black/12 text-sm text-slate-500">
            {t("stack.emptyHint")}
          </div>
        ) : (
          <div className="flex min-h-full flex-col gap-4">
            {sections.commander.length > 0 && (
              <DeckStackSectionLane
                title={t("stack.commanderLane")}
                badge={t("stack.cardCount", { count: sections.commander.length })}
                entries={sections.commander}
                emptyLabel={t("stack.noCommander")}
                isMaybeboard={isMaybeboard}
                onAddCard={onAddCard}
                canAddCard={canAddCard}
                onRemoveCard={onRemoveCard}
                onMoveCard={onMoveCard}
                onRemoveCommander={onRemoveCommander}
                onCardHover={onCardHover}
                onContextMenu={handleContextMenu}
              />
            )}
            <DeckStackSectionLane
              title={t("stack.mainDeckLane")}
              badge={
                sideboardCount > 0
                  ? isMaybeboard
                    ? t("stack.mainMaybeBadge", { main: mainDeckCount, side: sideboardCount })
                    : t("stack.mainSideBadge", { main: mainDeckCount, side: sideboardCount })
                  : t("stack.cardCount", { count: mainDeckCount })
              }
              entries={sections.main}
              emptyLabel={t("stack.mainEmpty")}
              showGroupSections
              extraGroups={sideboardGroups}
              isMaybeboard={isMaybeboard}
              onAddCard={onAddCard}
              canAddCard={canAddCard}
              onRemoveCard={onRemoveCard}
              onMoveCard={onMoveCard}
              onRemoveCommander={onRemoveCommander}
              onCardHover={onCardHover}
              onContextMenu={handleContextMenu}
            />
          </div>
        )}
      </div>

      {contextMenu && (
        <DeckCardContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          cardName={contextMenu.cardName}
          hasOverride={!!artOverrides[resolveOracleIdSync(contextMenu.cardName) ?? ""]}
          hasAlternates={hasAlternatePrintingsSync(resolveOracleIdSync(contextMenu.cardName) ?? "")}
          onChooseArt={handleChooseArt}
          onClearOverride={handleClearOverride}
          onClose={() => setContextMenu(null)}
        />
      )}

      {pickerCard && (
        <PrintingPickerModal
          cardName={pickerCard.cardName}
          oracleId={pickerCard.oracleId}
          onCardHover={onCardHover}
          onClose={() => setPickerCard(null)}
        />
      )}
    </div>
  );
}
