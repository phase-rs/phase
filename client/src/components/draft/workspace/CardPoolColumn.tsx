import { type KeyboardEvent, type RefCallback } from "react";
import { useTranslation } from "react-i18next";

import type { DraftPoolGroupKind } from "../../../adapter/draft-adapter";
import { ManaFontIcon } from "../../icons/ManaFontIcon";
import { ManaSymbol } from "../../mana/ManaSymbol";
import type {
  WorkspaceBoardColumnModel,
  WorkspaceCardEntryModel,
  WorkspaceHeaderDescriptor,
} from "./workspacePlacement";
import type {
  DraftWorkspaceDragController,
  WorkspaceDragSource,
} from "./useDraftWorkspaceDrag";
import { WorkspaceCard } from "./WorkspaceCard";
import type { DraftBoardSort } from "./workspacePreferences";

const SORT_GROUPS: Record<DraftBoardSort, ReadonlySet<DraftPoolGroupKind>> = {
  cmc: new Set(["mana_value0", "mana_value1", "mana_value2", "mana_value3", "mana_value4", "mana_value5", "mana_value6_plus"]),
  color: new Set(["white", "blue", "black", "red", "green", "multicolor", "colorless"]),
  rarity: new Set(["mythic", "rare", "uncommon", "common", "rarity_other"]),
  type: new Set(["creature", "instant", "sorcery", "enchantment", "artifact", "planeswalker", "land", "other"]),
};

interface CardPoolColumnProps {
  column: WorkspaceBoardColumnModel;
  sort: DraftBoardSort;
  interactionLocked?: boolean;
  dragController?: DraftWorkspaceDragController;
  touchDragEnabled?: boolean;
  touchScrollEnabled?: boolean;
  registerCardArea?: RefCallback<HTMLElement>;
  showHeader: boolean;
  canRemove: boolean;
  registerCard(instanceId: string): RefCallback<HTMLButtonElement>;
  registerHeader(column: number): RefCallback<HTMLElement>;
  makeDragSource(card: WorkspaceCardEntryModel, width: number, height: number): WorkspaceDragSource;
  onRemoveColumn(column: number): void;
  onCardHover(card: WorkspaceCardEntryModel | null): void;
  onCardActivate(card: WorkspaceCardEntryModel): void;
  onCardKeyDown(event: KeyboardEvent<HTMLButtonElement>, card: WorkspaceCardEntryModel): void;
}

interface LocaleListFormatter {
  format(values: readonly string[]): string;
}

type IntlWithListFormat = typeof Intl & {
  ListFormat: new (
    locales?: string | readonly string[],
    options?: { style: "long"; type: "conjunction" },
  ) => LocaleListFormatter;
};

function descriptorLabel(
  descriptor: WorkspaceHeaderDescriptor,
  t: ReturnType<typeof useTranslation>["t"],
): string {
  if (descriptor.kind === "empty-ordinal") {
    return t(descriptor.labelKey, { column: descriptor.ordinal });
  }
  if (descriptor.kind === "mana-value-column") {
    return t(descriptor.labelKey, { bucket: descriptor.manaValue });
  }
  return t(descriptor.labelKey);
}

function HeaderPresentation({
  descriptor,
  label,
}: {
  descriptor: WorkspaceHeaderDescriptor;
  label: string;
}) {
  if (descriptor.kind !== "engine-group" && descriptor.kind !== "mana-value-column") return null;
  const presentation = descriptor.presentation;
  switch (presentation.kind) {
    case "mana-symbol":
      return (
        <span aria-hidden="true">
          <ManaSymbol shard={presentation.shard} size="sm" className="!h-[17px] !w-[17px] shrink-0" />
        </span>
      );
    case "mana-font":
      return (
        <ManaFontIcon
          iconClass={presentation.iconClass}
          fallbackText={presentation.fallbackText}
          size="sm"
          className={presentation.iconClass.startsWith("ms-multicolor")
            ? "h-[17px] w-[17px] shrink-0 !text-[17px]"
            : "shrink-0"}
        />
      );
    case "numeric-badge": {
      const hasPlus = presentation.text.endsWith("+");
      const manaValue = hasPlus ? presentation.text.slice(0, -1) : presentation.text;
      return (
        <span
          aria-hidden="true"
          data-mana-value-badge={presentation.text}
          className="relative inline-flex shrink-0"
        >
          <ManaSymbol shard={manaValue} size="sm" className="!h-[17px] !w-[17px] shrink-0" />
          {hasPlus && (
            <span className="absolute -right-1 -top-1 flex h-2.5 w-2.5 items-center justify-center rounded-full bg-slate-950 text-[8px] font-bold leading-none text-white ring-1 ring-white/30">
              +
            </span>
          )}
        </span>
      );
    }
    case "text-only":
      return <span className="truncate">{label}</span>;
  }
}

export function CardPoolColumn({
  column,
  sort,
  interactionLocked = false,
  dragController,
  touchDragEnabled = false,
  touchScrollEnabled = false,
  registerCardArea,
  showHeader,
  canRemove,
  registerCard,
  registerHeader,
  makeDragSource,
  onRemoveColumn,
  onCardHover,
  onCardActivate,
  onCardKeyDown,
}: CardPoolColumnProps) {
  const { t, i18n } = useTranslation("draft");
  const labels = column.header.descriptors.map((descriptor) => descriptorLabel(descriptor, t));
  const labelList = new (Intl as IntlWithListFormat).ListFormat(i18n.language, {
    style: "long",
    type: "conjunction",
  }).format(labels);
  const headerName = t("workspace.headers.accessible", {
    column: column.column + 1,
    labels: labelList,
    count: column.header.count,
  });
  const visualDescriptors = column.header.descriptors.filter((descriptor) => (
    sort === "cmc"
      ? descriptor.kind === "mana-value-column"
      : descriptor.kind === "engine-group" && SORT_GROUPS[sort].has(descriptor.groupKind)
  ));
  const hasIncompatibleDescriptor = column.header.descriptors.some((descriptor) => (
    sort === "cmc" ? descriptor.kind !== "mana-value-column" : descriptor.kind !== "engine-group"
  ));
  const visualDescriptor = !hasIncompatibleDescriptor && visualDescriptors.length === 1
    ? visualDescriptors[0]
    : null;

  return (
    <section
      style={column.rows.length === 2 ? { borderColor: "transparent", gridTemplateRows: "subgrid" } : undefined}
      className={`h-full min-w-0 select-none overflow-visible border caret-transparent ${column.rows.length === 2
        ? "row-span-3 grid grid-rows-subgrid bg-transparent shadow-none"
        : "flex flex-col rounded-[8px] border-hairline bg-black/28 shadow-[inset_0_1px_0_rgba(255,255,255,0.035)] transition-colors hover:border-hairline-hover"
      }`}
      data-board-column={column.column}
      data-drop-state={column.drop.state}
      aria-describedby={column.drop.active ? `${column.key}:drop-description` : undefined}
    >
      {column.drop.active && (
        <span
          id={`${column.key}:drop-description`}
          className="sr-only"
        >
          {t(column.drop.descriptionKey!)}
        </span>
      )}
      {showHeader && (
        <header
          ref={registerHeader(column.column)}
          tabIndex={-1}
          className="relative z-10 row-start-1 flex h-8 min-h-8 items-center gap-1 overflow-hidden whitespace-nowrap rounded-t-[7px] border-b border-hairline bg-white/[0.045] px-2 text-xs text-fg-muted"
          aria-label={headerName}
        >
          <span data-card-count aria-hidden="true" className="shrink-0 font-mono text-sm tabular-nums text-fg">{column.header.count}</span>
          {visualDescriptor !== null && (
            <span
              data-sort-designation
              className="absolute left-1/2 inline-flex max-w-[calc(100%-4rem)] -translate-x-1/2 items-center justify-center overflow-hidden"
              title={descriptorLabel(visualDescriptor, t)}
            >
              <HeaderPresentation
                descriptor={visualDescriptor}
                label={descriptorLabel(visualDescriptor, t)}
              />
            </span>
          )}
          <button
            type="button"
            disabled={interactionLocked || !canRemove}
            title={t("workspace.columns.removeHeader", { column: column.column + 1 })}
            onClick={() => onRemoveColumn(column.column)}
            aria-label={t("workspace.columns.removeHeader", { column: column.column + 1 })}
            className="ml-auto h-6 w-6 shrink-0 rounded-[6px] text-base text-fg-meta transition-colors hover:bg-white/[0.07] hover:text-rose disabled:cursor-not-allowed disabled:opacity-35"
          >
            −
          </button>
        </header>
      )}
      <div
        ref={registerCardArea}
        data-card-area
        className={`${column.rows.length === 2
          ? `relative row-start-2 row-span-2 grid min-h-0 min-w-0 grid-rows-subgrid ${showHeader ? "rounded-b-[8px]" : "rounded-[8px]"} border border-hairline bg-black/28`
          : `relative grid min-h-0 flex-1 gap-2 ${showHeader ? "rounded-b-[7px]" : "rounded-[7px]"}`
        } ${column.drop.active ? "draft-card-area-drop-active" : ""}`}
        style={column.rows.length === 2
          ? { gridTemplateRows: "subgrid" }
          : { gridTemplateRows: `repeat(${column.rows.length}, minmax(0, 1fr))` }
        }
      >
        {column.rows.map((row) => (
            <div
              key={row.key}
              className={`${column.rows.length === 2 ? `${row.row === 1 ? "mt-2 rounded-[7px]" : ""} border border-hairline` : ""} relative grid min-w-0`}
              style={column.rows.length === 2 ? { gridRow: row.row + 1 } : undefined}
              data-board-row={row.row}
              data-drop-state={row.drop.state}
              aria-describedby={row.drop.active ? `${row.key}:drop-description` : undefined}
            >
              <span aria-hidden="true" data-card-height-baseline className="block aspect-[488/680] w-full self-start [grid-area:1/1]" />
              {row.drop.active && (
                <span id={`${row.key}:drop-description`} className="sr-only">
                  {t(row.drop.descriptionKey!)}
                </span>
              )}
              <div data-card-stack className="min-w-0 [grid-area:1/1]">
                {row.cards.map((card, stackIndex) => (
                  <WorkspaceCard
                    key={card.key}
                    card={card}
                    stackIndex={stackIndex}
                    registerCard={registerCard(card.key)}
                    onHover={onCardHover}
                    onActivate={onCardActivate}
                    onKeyDown={onCardKeyDown}
                    interactionLocked={interactionLocked}
                    drag={dragController === undefined
                      ? undefined
                      : {
                        controller: dragController,
                        makeSource: makeDragSource,
                        touchDragEnabled,
                        touchScrollEnabled,
                      }}
                  />
                ))}
              </div>
            </div>
        ))}
      </div>
    </section>
  );
}