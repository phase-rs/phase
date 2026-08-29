import type { ReactNode, Ref } from "react";
import { useTranslation } from "react-i18next";

import type { DraftWorkspaceCapabilities } from "../../../adapter/draft-adapter";
import { PopoverMenu, popoverMenuItemClass } from "../../menu/PopoverMenu";
import { DeckTypeCounts } from "./DeckTypeCounts";
import {
  DRAFT_WORKSPACE_COLUMN_MAX,
  DRAFT_WORKSPACE_COLUMN_MIN,
  type DraftBoardPreferences,
  type DraftBoardRows,
  type DraftBoardSort,
} from "./workspacePreferences";

interface DraftWorkspaceToolbarProps {
  heading?: string;
  deckTypeCounts?: { creatures: number; lands: number };
  deckControls?: ReactNode;
  trailingControls?: ReactNode;
  preferences: DraftBoardPreferences;
  capabilities: DraftWorkspaceCapabilities;
  minusRef?: Ref<HTMLButtonElement>;
  interactionLocked?: boolean;
  phoneMode?: boolean;
  phoneLayoutDialog?: boolean;
  phonePortraitDeckToolbar?: boolean;
  tabletMode?: boolean;
  compactDeckTypeCounts?: boolean;
  visualColumnCapValue?: number;
  visualColumnCapMax?: number;
  onVisualColumnCapChange?(next: number): void;
  onChange(next: DraftBoardPreferences): void;
}

export function DraftWorkspaceToolbar({
  heading,
  deckTypeCounts,
  deckControls,
  trailingControls,
  preferences,
  capabilities,
  minusRef,
  interactionLocked = false,
  phoneMode = false,
  phoneLayoutDialog = false,
  phonePortraitDeckToolbar = false,
  tabletMode = false,
  compactDeckTypeCounts = false,
  visualColumnCapValue,
  visualColumnCapMax,
  onVisualColumnCapChange,
  onChange,
}: DraftWorkspaceToolbarProps) {
  const { t } = useTranslation("draft");
  const sorts: DraftBoardSort[] = capabilities.rarity_group_order === null
    ? ["cmc", "color", "type"]
    : ["cmc", "color", "rarity", "type"];
  const setRows = (rows: DraftBoardRows) => onChange({ ...preferences, rows });
  const rowControls = (
    <div
      role="group"
      aria-label={t("workspace.rows.label")}
      className="inline-flex overflow-hidden rounded-[8px] border border-hairline bg-black/30 p-1"
    >
      {(["one", "two"] as const).map((rows) => (
        <button
          key={rows}
          type="button"
          disabled={interactionLocked}
          aria-pressed={preferences.rows === rows}
          onClick={() => setRows(rows)}
          className={`min-h-9 rounded-[6px] px-3 text-sm font-medium transition-colors ${preferences.rows === rows ? "bg-jade/15 text-jade-text ring-1 ring-jade/35" : "text-fg-muted hover:bg-white/[0.06] hover:text-fg"}`}
        >
          {t(`workspace.rows.${rows}`)}
        </button>
      ))}
    </div>
  );
  const showHeadersControl = (
    <label className="inline-flex min-h-9 items-center gap-2 text-sm text-fg-muted">
      <input
        type="checkbox"
        className="accent-jade"
        disabled={interactionLocked}
        checked={preferences.showHeaders}
        onChange={(event) => onChange({ ...preferences, showHeaders: event.target.checked })}
      />
      {t(tabletMode ? "workspace.headers.compact" : "workspace.headers.show")}
    </label>
  );
  const columnControls = (
    <div role="group" aria-label={t("workspace.columns.label")} className="inline-flex items-center gap-1 rounded-[8px] border border-hairline bg-black/30 p-1">
      <button
        ref={minusRef}
        type="button"
        disabled={interactionLocked || preferences.columnCount <= DRAFT_WORKSPACE_COLUMN_MIN}
        onClick={() => onChange({ ...preferences, columnCount: preferences.columnCount - 1 })}
        aria-label={t("workspace.columns.removeFinal")}
        title={t("workspace.columns.removeFinal")}
        className="h-9 w-9 rounded-[6px] text-xl text-fg-muted transition-colors hover:bg-white/[0.06] hover:text-fg disabled:cursor-not-allowed disabled:opacity-35"
      >
        −
      </button>
      <output className="min-w-8 text-center font-mono text-sm text-fg" aria-live="polite">
        {preferences.columnCount}
      </output>
      <button
        type="button"
        disabled={interactionLocked || preferences.columnCount >= DRAFT_WORKSPACE_COLUMN_MAX}
        onClick={() => onChange({ ...preferences, columnCount: preferences.columnCount + 1 })}
        aria-label={t("workspace.columns.add")}
        title={t("workspace.columns.add")}
        className="h-9 w-9 rounded-[6px] text-xl text-fg-muted transition-colors hover:bg-white/[0.06] hover:text-fg disabled:cursor-not-allowed disabled:opacity-35"
      >
        +
      </button>
    </div>
  );
  const layoutColumnControls = (
    <div role="group" aria-label={t("workspace.columns.label")} className="flex items-center justify-center gap-2">
      <button
        ref={minusRef}
        type="button"
        disabled={interactionLocked || preferences.columnCount <= DRAFT_WORKSPACE_COLUMN_MIN}
        onClick={() => onChange({ ...preferences, columnCount: preferences.columnCount - 1 })}
        aria-label={t("workspace.columns.removeFinal")}
        className="h-8 w-8 rounded-[6px] border border-hairline text-lg text-fg-muted disabled:cursor-not-allowed disabled:opacity-35"
      >
        −
      </button>
      <output className="min-w-8 text-center font-mono text-sm text-fg" aria-live="polite">
        {preferences.columnCount}
      </output>
      <button
        type="button"
        disabled={interactionLocked || preferences.columnCount >= DRAFT_WORKSPACE_COLUMN_MAX}
        onClick={() => onChange({ ...preferences, columnCount: preferences.columnCount + 1 })}
        aria-label={t("workspace.columns.add")}
        className="h-8 w-8 rounded-[6px] border border-hairline text-lg text-fg-muted disabled:cursor-not-allowed disabled:opacity-35"
      >
        +
      </button>
    </div>
  );
  const layoutCapControls = visualColumnCapValue === undefined
    || visualColumnCapMax === undefined
    || onVisualColumnCapChange === undefined
    ? null
    : (
      <div role="group" aria-label={t("workspace.layout.maxPerRow")} className="flex items-center justify-center gap-2">
        <button
          type="button"
          disabled={interactionLocked || visualColumnCapValue <= 1}
          onClick={() => onVisualColumnCapChange(visualColumnCapValue - 1)}
          aria-label={t("workspace.layout.decreaseMaxPerRow")}
          className="h-11 w-11 rounded-[6px] border border-hairline text-lg text-fg-muted disabled:cursor-not-allowed disabled:opacity-35"
        >
          −
        </button>
        <output className="min-w-8 text-center font-mono text-sm text-fg" aria-live="polite">
          {visualColumnCapValue}
        </output>
        <button
          type="button"
          disabled={interactionLocked || visualColumnCapValue >= visualColumnCapMax}
          onClick={() => onVisualColumnCapChange(visualColumnCapValue + 1)}
          aria-label={t("workspace.layout.increaseMaxPerRow")}
          className="h-11 w-11 rounded-[6px] border border-hairline text-lg text-fg-muted disabled:cursor-not-allowed disabled:opacity-35"
        >
          +
        </button>
      </div>
    );

  return (
    <div
      role="toolbar"
      aria-label={t("workspace.toolbar.label")}
      className={`flex flex-wrap items-center gap-3 border-b border-hairline ${phonePortraitDeckToolbar ? "px-2" : "px-4"} py-1.5 shadow-[inset_0_-1px_0_rgba(0,0,0,0.2)] ${phoneMode ? "sticky top-0 z-20 bg-slate-950" : "bg-white/[0.035]"}`}
    >
      {heading !== undefined && (
        <h2 className="shrink-0 font-display text-base font-semibold text-fg">{heading}</h2>
      )}
      {phoneLayoutDialog ? (
        <PopoverMenu
          ariaLabel={t("workspace.layout.label")}
          variant="dialog"
          menuWidthPx={256}
          renderTrigger={({ ref, open, toggle }) => (
            <button
              ref={ref}
              type="button"
              aria-haspopup="dialog"
              aria-expanded={open}
              aria-label={t("workspace.layout.label")}
              disabled={interactionLocked}
              onClick={toggle}
              className="inline-flex min-h-9 items-center gap-2 rounded-[6px] border border-hairline bg-slate-950/72 px-3 text-sm text-fg transition-colors hover:border-hairline-hover hover:bg-slate-900/88 disabled:cursor-not-allowed disabled:opacity-40"
            >
              <span>{t("workspace.layout.label")}</span>
              <span aria-hidden="true">▼</span>
            </button>
          )}
        >
          {(close) => (
            <div className="flex flex-col gap-2 p-2">
              <h3 className="text-center text-sm font-semibold text-fg">{t("workspace.layout.columns")}</h3>
              {layoutColumnControls}
              <div className="border-t border-hairline pt-2">
                <p className="mb-1 text-center text-sm font-semibold text-fg">{t("workspace.layout.maxPerRow")}</p>
                {layoutCapControls}
              </div>
              <div className="border-t border-hairline pt-2">
                <h3 className="mb-1 text-center text-sm font-semibold text-fg">{t("workspace.sort.label")}</h3>
                <div data-layout-sort-options className="grid grid-cols-2 gap-1.5">
                  {sorts.map((sort) => (
                    <button
                      key={sort}
                      type="button"
                      aria-pressed={preferences.sort === sort}
                      className={`${popoverMenuItemClass} justify-center text-center`}
                      onClick={() => {
                        onChange({ ...preferences, sort });
                        close();
                      }}
                    >
                      {t(`workspace.sort.${sort}`)}
                    </button>
                  ))}
                </div>
              </div>
            </div>
          )}
        </PopoverMenu>
      ) : phoneMode ? (
        <PopoverMenu
          ariaLabel={t("workspace.sort.label")}
          menuWidthPx={176}
          renderTrigger={({ ref, open, toggle }) => (
            <button
              ref={ref}
              type="button"
              aria-haspopup="menu"
              aria-expanded={open}
              aria-label={t("workspace.sort.label")}
              disabled={interactionLocked}
              onClick={toggle}
              className="inline-flex min-h-9 items-center gap-2 rounded-[6px] border border-hairline bg-slate-950/72 px-3 text-sm text-fg transition-colors hover:border-hairline-hover hover:bg-slate-900/88 disabled:cursor-not-allowed disabled:opacity-40"
            >
              <span>{t("workspace.sort.label")}</span>
              <span aria-hidden="true">▼</span>
            </button>
          )}
        >
          {(close) => sorts.map((sort) => (
            <button
              key={sort}
              type="button"
              role="menuitemradio"
              aria-checked={preferences.sort === sort}
              className={popoverMenuItemClass}
              onClick={() => {
                onChange({ ...preferences, sort });
                close();
              }}
            >
              {t(`workspace.sort.${sort}`)}
            </button>
          ))}
        </PopoverMenu>
      ) : (
        <>
          <label className="flex min-w-0 max-w-full items-center gap-2 text-sm text-fg-muted">
            <span className="shrink-0">{t("workspace.sort.label")}</span>
            <select
              aria-label={t("workspace.sort.label")}
              title={t("workspace.sort.label")}
              value={preferences.sort}
              className="min-h-9 min-w-0 max-w-48 rounded-[8px] border border-hairline bg-slate-950/72 px-3 text-sm text-fg transition-colors hover:border-hairline-hover hover:bg-slate-900/88 focus:border-jade/40 focus:outline-none focus:ring-2 focus:ring-jade/20"
              onChange={(event) => onChange({ ...preferences, sort: event.target.value as DraftBoardSort })}
              disabled={interactionLocked}
            >
              {sorts.map((sort) => (
                <option key={sort} value={sort}>{t(`workspace.sort.${sort}`)}</option>
              ))}
            </select>
          </label>
          {rowControls}
          {columnControls}
          {showHeadersControl}
        </>
      )}
      {phoneLayoutDialog && tabletMode && showHeadersControl}
      {phoneMode && !phoneLayoutDialog && columnControls}
      {deckTypeCounts !== undefined && (
        <DeckTypeCounts counts={deckTypeCounts} compact={compactDeckTypeCounts} />
      )}
      {deckControls}
      {trailingControls}
    </div>
  );
}
