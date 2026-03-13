import { useMemo } from "react";

import { useCardImage } from "../../hooks/useCardImage";
import type { DeckEntry, ParsedDeck } from "../../services/deckParser";
import type { ScryfallCard } from "../../services/scryfall";

interface DeckStackProps {
  deck: ParsedDeck;
  commanders: string[];
  cardDataCache: Map<string, ScryfallCard>;
  onAddCard: (name: string) => void;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onRemoveCommander: (cardName: string) => void;
  onCardHover?: (cardName: string | null) => void;
}

type DeckStackSection = "commander" | "main" | "sideboard";

interface DeckStackItem {
  count: number;
  name: string;
  section: DeckStackSection;
  typeRank: number;
  sortKey: [number, number, string];
}

interface DeckStackTypeGroup {
  key: string;
  title: string;
  entries: DeckStackItem[];
}

const CARD_HEIGHT = 156;
const CARD_WIDTH = 112;

function getTypeRank(card: ScryfallCard | undefined): number {
  const typeLine = card?.type_line.toLowerCase() ?? "";
  if (typeLine.includes("land")) return 2;
  if (typeLine.includes("creature")) return 0;
  return 1;
}

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
): Record<DeckStackSection, DeckStackItem[]> {
  const commandersItems: DeckStackItem[] = [];
  for (const name of commanders) {
    const card = cardDataCache.get(name);
    commandersItems.push({
      count: 1,
      name,
      section: "commander",
      typeRank: 0,
      sortKey: [0, card?.cmc ?? 0, name.toLowerCase()],
    });
  }

  const mainItems: DeckStackItem[] = [];
  for (const entry of deck.main) {
    const card = cardDataCache.get(entry.name);
    const typeRank = getTypeRank(card);
    mainItems.push({
      count: entry.count,
      name: entry.name,
      section: "main",
      typeRank,
      sortKey: [1 + typeRank, card?.cmc ?? 0, entry.name.toLowerCase()],
    });
  }

  const sideboardItems: DeckStackItem[] = [];
  for (const entry of deck.sideboard) {
    const card = cardDataCache.get(entry.name);
    const typeRank = getTypeRank(card);
    sideboardItems.push({
      count: entry.count,
      name: entry.name,
      section: "sideboard",
      typeRank,
      sortKey: [4 + typeRank, card?.cmc ?? 0, entry.name.toLowerCase()],
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

function buildTypeGroups(entries: DeckStackItem[]): DeckStackTypeGroup[] {
  if (entries.length === 0) return [];

  const groups: DeckStackTypeGroup[] = [];
  let currentRank: number | null = null;
  let currentEntries: DeckStackItem[] = [];

  const flush = () => {
    if (currentRank === null || currentEntries.length === 0) return;
    groups.push({
      key: `type-${currentRank}`,
      title: currentRank === 0 ? "Creatures" : currentRank === 1 ? "Spells" : "Lands",
      entries: currentEntries,
    });
  };

  for (const entry of entries) {
    if (currentRank !== entry.typeRank) {
      flush();
      currentRank = entry.typeRank;
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
  onAddCard,
  onRemoveCard,
  onRemoveCommander,
  onCardHover,
}: {
  item: DeckStackItem;
  zIndex: number;
  className?: string;
  canAdd: boolean;
  onAddCard: (name: string) => void;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onRemoveCommander: (cardName: string) => void;
  onCardHover?: (cardName: string | null) => void;
}) {
  const { src, isLoading } = useCardImage(item.name, { size: "normal" });
  const isCommander = item.section === "commander";
  const showAddButton = item.section === "main";

  const handleRemove = () => {
    if (item.section === "commander") {
      onRemoveCommander(item.name);
      return;
    }
    onRemoveCard(item.name, item.section);
  };

  const handleAdd = () => {
    if (!canAdd) return;
    onAddCard(item.name);
  };

  return (
    <div
      className={`relative ${className ?? ""}`}
      style={{ zIndex, width: CARD_WIDTH }}
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
          {isCommander && (
            <span className="rounded-full bg-fuchsia-200/95 px-2 py-0.5 text-[10px] font-bold text-fuchsia-950">
              Commander
            </span>
          )}
        </div>
        {showAddButton && (
          <button
            onClick={handleAdd}
            disabled={!canAdd}
            className="absolute right-10 top-2 z-10 flex h-6 w-6 items-center justify-center rounded-full bg-black/78 text-sm font-bold text-emerald-300 opacity-0 transition group-hover:opacity-100 hover:bg-emerald-500/85 hover:text-white disabled:cursor-not-allowed disabled:text-slate-500 disabled:hover:bg-black/78"
            title={canAdd ? `Add one ${item.name}` : `${item.name} is at the copy limit`}
          >
            +
          </button>
        )}
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
  showTypeSections = false,
  onAddCard,
  canAddCard,
  onRemoveCard,
  onRemoveCommander,
  onCardHover,
}: {
  title: string;
  badge: string;
  entries: DeckStackItem[];
  emptyLabel: string;
  showTypeSections?: boolean;
  onAddCard: (name: string) => void;
  canAddCard: (item: DeckStackItem) => boolean;
  onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  onRemoveCommander: (cardName: string) => void;
  onCardHover?: (cardName: string | null) => void;
}) {
  const typeGroups = useMemo(
    () => showTypeSections ? buildTypeGroups(entries) : [{ key: "all", title: "", entries }],
    [entries, showTypeSections],
  );

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
        <div className="overflow-auto pb-1">
          <div className="flex min-w-0 flex-col gap-6">
            {typeGroups.map((group, groupIndex) => (
              <div key={group.key} className={groupIndex > 0 ? "pt-1" : undefined}>
                {showTypeSections && (
                  <div className="mb-3 flex items-center gap-3">
                    <div className="text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-slate-500">
                      {group.title}
                    </div>
                    <div className="h-px flex-1 bg-white/8" />
                    <div className="text-[11px] text-slate-500">
                      {group.entries.reduce((sum, entry) => sum + entry.count, 0)} cards
                    </div>
                  </div>
                )}
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
                      onAddCard={onAddCard}
                      onRemoveCard={onRemoveCard}
                      onRemoveCommander={onRemoveCommander}
                      onCardHover={onCardHover}
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
  const canAddCard = useMemo(
    () => (item: DeckStackItem) => {
      if (item.section !== "main") return false;
      const typeLine = cardDataCache.get(item.name)?.type_line.toLowerCase() ?? "";
      const isBasicLand = typeLine.includes("basic") && typeLine.includes("land");
      return isBasicLand || item.count < 4;
    },
    [cardDataCache],
  );

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
                onAddCard={onAddCard}
                canAddCard={canAddCard}
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
              showTypeSections
              onAddCard={onAddCard}
              canAddCard={canAddCard}
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
                onAddCard={onAddCard}
                canAddCard={canAddCard}
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
