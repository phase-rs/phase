import { useEffect, useMemo, useRef, useState } from "react";

import { useCardImage } from "../../hooks/useCardImage";
import type { DeckEntry, ParsedDeck } from "../../services/deckParser";
import type { ScryfallCard } from "../../services/scryfall";

interface DeckStackProps {
  deck: ParsedDeck;
  commanders: string[];
  cardDataCache: Map<string, ScryfallCard>;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onRemoveCommander: (cardName: string) => void;
  onCardHover?: (cardName: string | null) => void;
}

type DeckStackSection = "commander" | "main" | "sideboard";

interface DeckStackItem {
  count: number;
  name: string;
  section: DeckStackSection;
  sortKey: [number, number, number, string];
}

const CARD_HEIGHT = 156;
const CARD_WIDTH = 112;
const CARD_OFFSET = 48;
const COLUMN_GAP = 16;
const TARGET_ITEMS_PER_COLUMN = 5;
const MIN_ITEMS_PER_COLUMN = 4;

const SECTION_LABELS: Record<DeckStackSection, string> = {
  commander: "CMD",
  main: "MD",
  sideboard: "SB",
};

const SECTION_STYLES: Record<DeckStackSection, string> = {
  commander: "bg-fuchsia-500/90 text-black",
  main: "bg-slate-900/90 text-slate-100",
  sideboard: "bg-amber-300/90 text-black",
};

function getTypeRank(card: ScryfallCard | undefined): number {
  const typeLine = card?.type_line.toLowerCase() ?? "";
  if (typeLine.includes("land")) return 2;
  if (typeLine.includes("creature")) return 0;
  return 1;
}

function sortDeckStackItems(items: DeckStackItem[]): DeckStackItem[] {
  const next = [...items];
  next.sort((left, right) => {
    const [leftRank, leftCmc, leftCountOrder, leftName] = left.sortKey;
    const [rightRank, rightCmc, rightCountOrder, rightName] = right.sortKey;
    if (leftRank !== rightRank) return leftRank - rightRank;
    if (leftCmc !== rightCmc) return leftCmc - rightCmc;
    if (leftCountOrder !== rightCountOrder) return leftCountOrder - rightCountOrder;
    return leftName.localeCompare(rightName);
  });
  return next;
}

function createDeckStackItems(
  deck: ParsedDeck,
  commanders: string[],
  cardDataCache: Map<string, ScryfallCard>,
): Record<DeckStackSection, DeckStackItem[]> {
  const commandersItems: DeckStackItem[] = [];
  for (const name of commanders) {
    const card = cardDataCache.get(name);
    commandersItems.push({
      count: 1,
      name,
      section: "commander",
      sortKey: [0, card?.cmc ?? 0, -1, name.toLowerCase()],
    });
  }

  const mainItems: DeckStackItem[] = [];
  for (const entry of deck.main) {
    const card = cardDataCache.get(entry.name);
    mainItems.push({
      count: entry.count,
      name: entry.name,
      section: "main",
      sortKey: [1 + getTypeRank(card), card?.cmc ?? 0, -entry.count, entry.name.toLowerCase()],
    });
  }

  const sideboardItems: DeckStackItem[] = [];
  for (const entry of deck.sideboard) {
    const card = cardDataCache.get(entry.name);
    sideboardItems.push({
      count: entry.count,
      name: entry.name,
      section: "sideboard",
      sortKey: [4 + getTypeRank(card), card?.cmc ?? 0, -entry.count, entry.name.toLowerCase()],
    });
  }

  return {
    commander: sortDeckStackItems(commandersItems),
    main: sortDeckStackItems(mainItems),
    sideboard: sortDeckStackItems(sideboardItems),
  };
}

function chunkEntries(entries: DeckStackItem[], availableWidth: number): DeckStackItem[][] {
  if (entries.length === 0) return [];

  const minimumColumnCount = Math.max(
    1,
    Math.ceil(entries.length / TARGET_ITEMS_PER_COLUMN),
  );
  const maximumColumnCount = Math.max(
    1,
    Math.ceil(entries.length / MIN_ITEMS_PER_COLUMN),
  );
  const visibleColumnCount = availableWidth > 0
    ? Math.max(
      1,
      Math.floor((availableWidth + COLUMN_GAP) / (CARD_WIDTH + COLUMN_GAP)),
    )
    : minimumColumnCount;
  const columnCount = Math.min(
    entries.length,
    Math.max(
      minimumColumnCount,
      Math.min(visibleColumnCount, maximumColumnCount),
    ),
  );

  const baseColumnSize = Math.floor(entries.length / columnCount);
  const columnsWithExtraCard = entries.length % columnCount;
  const columns: DeckStackItem[][] = [];
  let offset = 0;

  for (let columnIndex = 0; columnIndex < columnCount; columnIndex += 1) {
    const columnSize = baseColumnSize + (columnIndex < columnsWithExtraCard ? 1 : 0);
    columns.push(entries.slice(offset, offset + columnSize));
    offset += columnSize;
  }

  return columns;
}

function totalCards(entries: DeckEntry[]): number {
  return entries.reduce((sum, entry) => sum + entry.count, 0);
}

function DeckStackCard({
  item,
  top,
  zIndex,
  onRemoveCard,
  onRemoveCommander,
  onCardHover,
}: {
  item: DeckStackItem;
  top: number;
  zIndex: number;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onRemoveCommander: (cardName: string) => void;
  onCardHover?: (cardName: string | null) => void;
}) {
  const { src, isLoading } = useCardImage(item.name, { size: "normal" });
  const isCommander = item.section === "commander";

  const handleRemove = () => {
    if (item.section === "commander") {
      onRemoveCommander(item.name);
      return;
    }
    onRemoveCard(item.name, item.section);
  };

  return (
    <div
      className="absolute left-0"
      style={{ top, zIndex, width: CARD_WIDTH }}
      onMouseEnter={() => onCardHover?.(item.name)}
      onMouseLeave={() => onCardHover?.(null)}
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
          <span
            className={`rounded-full px-2 py-0.5 text-[10px] font-semibold ${SECTION_STYLES[item.section]}`}
          >
            {SECTION_LABELS[item.section]}
          </span>
          {isCommander && (
            <span className="rounded-full bg-fuchsia-200/95 px-2 py-0.5 text-[10px] font-bold text-fuchsia-950">
              Commander
            </span>
          )}
        </div>
        <button
          onClick={handleRemove}
          className="absolute right-2 top-2 z-10 flex h-6 w-6 items-center justify-center rounded-full bg-black/78 text-sm font-bold text-red-300 opacity-0 transition group-hover:opacity-100 hover:bg-red-500/85 hover:text-white"
          title={
            item.section === "commander"
              ? `Remove ${item.name} as commander`
              : `Remove one ${item.name}`
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
        <div className="pointer-events-none absolute inset-x-0 bottom-0 bg-gradient-to-t from-black via-black/70 to-transparent px-2 pb-2 pt-8">
          <div className="truncate text-[11px] font-medium text-white">{item.name}</div>
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
  onRemoveCard,
  onRemoveCommander,
  onCardHover,
}: {
  title: string;
  badge: string;
  entries: DeckStackItem[];
  emptyLabel: string;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onRemoveCommander: (cardName: string) => void;
  onCardHover?: (cardName: string | null) => void;
}) {
  const laneRef = useRef<HTMLDivElement>(null);
  const [availableWidth, setAvailableWidth] = useState(0);
  const columns = useMemo(
    () => chunkEntries(entries, availableWidth),
    [entries, availableWidth],
  );

  useEffect(() => {
    const element = laneRef.current;
    if (!element) return;

    const updateWidth = (width: number) => {
      setAvailableWidth((currentWidth) => {
        const nextWidth = Math.floor(width);
        return currentWidth === nextWidth ? currentWidth : nextWidth;
      });
    };

    updateWidth(element.clientWidth);

    const observer = new ResizeObserver((observerEntries) => {
      const entry = observerEntries[0];
      updateWidth(entry?.contentRect.width ?? element.clientWidth);
    });

    observer.observe(element);
    return () => observer.disconnect();
  }, [entries.length]);

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
        <div ref={laneRef} className="overflow-auto pb-1">
          <div className="flex items-start gap-4">
            {columns.map((column, columnIndex) => (
              <div
                key={columnIndex}
                className="relative shrink-0"
                style={{
                  height: CARD_HEIGHT + Math.max(column.length - 1, 0) * CARD_OFFSET,
                  width: CARD_WIDTH,
                }}
              >
                {column.map((item, itemIndex) => (
                  <DeckStackCard
                    key={`${item.section}:${item.name}`}
                    item={item}
                    top={itemIndex * CARD_OFFSET}
                    zIndex={column.length - itemIndex}
                    onRemoveCard={onRemoveCard}
                    onRemoveCommander={onRemoveCommander}
                    onCardHover={onCardHover}
                  />
                ))}
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
  onRemoveCard,
  onRemoveCommander,
  onCardHover,
}: DeckStackProps) {
  const sections = useMemo(
    () => createDeckStackItems(deck, commanders, cardDataCache),
    [deck, commanders, cardDataCache],
  );
  const mainDeckCount = totalCards(deck.main) + commanders.length;
  const sideboardCount = totalCards(deck.sideboard);
  const hasCards =
    sections.commander.length > 0
    || sections.main.length > 0
    || sections.sideboard.length > 0;

  return (
    <div className="flex h-full min-h-0 flex-col overflow-hidden">
      <div className="flex items-center justify-between border-b border-white/8 px-3 py-2">
        <div>
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">Deck View</div>
          <div className="mt-1 text-sm font-semibold text-white">Visual Deck Stack</div>
        </div>
        <div className="flex items-center gap-2 text-[11px] text-slate-300">
          <span className="rounded-full border border-white/10 bg-black/20 px-2 py-1">
            Main {mainDeckCount}
          </span>
          {sideboardCount > 0 && (
            <span className="rounded-full border border-white/10 bg-black/20 px-2 py-1">
              Sideboard {sideboardCount}
            </span>
          )}
        </div>
      </div>

      <div className="flex-1 overflow-auto px-3 py-4">
        {!hasCards ? (
          <div className="flex h-full items-center justify-center rounded-[20px] border border-dashed border-white/10 bg-black/12 text-sm text-slate-500">
            Added cards will appear here as a staggered stack.
          </div>
        ) : (
          <div className="flex min-h-full flex-col gap-4">
            {sections.commander.length > 0 && (
              <DeckStackSectionLane
                title="Commander"
                badge={`${sections.commander.length} card${sections.commander.length === 1 ? "" : "s"}`}
                entries={sections.commander}
                emptyLabel="No commander selected."
                onRemoveCard={onRemoveCard}
                onRemoveCommander={onRemoveCommander}
                onCardHover={onCardHover}
              />
            )}
            <DeckStackSectionLane
              title="Main Deck"
              badge={`${mainDeckCount} cards`}
              entries={sections.main}
              emptyLabel="Main deck cards will appear here."
              onRemoveCard={onRemoveCard}
              onRemoveCommander={onRemoveCommander}
              onCardHover={onCardHover}
            />
            {sideboardCount > 0 && (
              <DeckStackSectionLane
                title="Sideboard"
                badge={`${sideboardCount} cards`}
                entries={sections.sideboard}
                emptyLabel="No sideboard cards."
                onRemoveCard={onRemoveCard}
                onRemoveCommander={onRemoveCommander}
                onCardHover={onCardHover}
              />
            )}
          </div>
        )}
      </div>
    </div>
  );
}
