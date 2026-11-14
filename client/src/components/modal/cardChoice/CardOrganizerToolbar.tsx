import { useTranslation } from "react-i18next";

import type { FilterKey, GroupKey, SortKey } from "./gridSelection.ts";

export interface CardOrganizerToolbarProps {
  sort: SortKey;
  onSortChange: (sort: SortKey) => void;
  group?: GroupKey;
  onGroupChange?: (group: GroupKey) => void;
  filter?: FilterKey;
  onFilterChange?: (filter: FilterKey) => void;
  query?: string;
  onQueryChange?: (query: string) => void;
  /** Which controls to render. Sort defaults on; the other axes default off so
   *  a caller opts into exactly the controls it needs. */
  showSort?: boolean;
  showGroup?: boolean;
  showFilter?: boolean;
  showQuery?: boolean;
  /** Disable every control (e.g. during target selection, when reorganizing the
   *  displayed order would be unsafe). */
  disabled?: boolean;
  className?: string;
}

const SELECT_CLASS =
  "rounded bg-black/40 px-1 py-0.5 disabled:cursor-not-allowed disabled:opacity-40";

/**
 * Presentational organizer controls for {@link useCardOrganizer}. Stateless —
 * it renders the current axis values and reports changes; the hook (or the
 * consuming component) owns the state. Shared across card-choice surfaces and
 * the player's hand so both expose one organizing mechanism.
 */
export function CardOrganizerToolbar({
  sort,
  onSortChange,
  group,
  onGroupChange,
  filter,
  onFilterChange,
  query,
  onQueryChange,
  showSort = true,
  showGroup = false,
  showFilter = false,
  showQuery = false,
  disabled = false,
  className = "flex flex-wrap items-center gap-2 text-xs text-slate-300",
}: CardOrganizerToolbarProps) {
  const { t } = useTranslation("game");
  return (
    <div className={className}>
      {showSort && (
        <label className="flex items-center gap-1">
          {t("cardChoice.bulk.sortLabel")}
          <select
            className={SELECT_CLASS}
            value={sort}
            disabled={disabled}
            onChange={(e) => onSortChange(e.target.value as SortKey)}
          >
            <option value="none">{t("cardChoice.bulk.optNone")}</option>
            <option value="name">{t("cardChoice.bulk.optName")}</option>
            <option value="cmc">{t("cardChoice.bulk.optCmc")}</option>
            <option value="type">{t("cardChoice.bulk.optType")}</option>
            <option value="color">{t("cardChoice.bulk.optColor")}</option>
          </select>
        </label>
      )}
      {showGroup && group !== undefined && onGroupChange && (
        <label className="flex items-center gap-1">
          {t("cardChoice.bulk.groupLabel")}
          <select
            className={SELECT_CLASS}
            value={group}
            disabled={disabled}
            onChange={(e) => onGroupChange(e.target.value as GroupKey)}
          >
            <option value="none">{t("cardChoice.bulk.optNone")}</option>
            <option value="type">{t("cardChoice.bulk.optType")}</option>
            <option value="color">{t("cardChoice.bulk.optColor")}</option>
          </select>
        </label>
      )}
      {showFilter && filter !== undefined && onFilterChange && (
        <label className="flex items-center gap-1">
          {t("cardChoice.bulk.filterLabel")}
          <select
            className={SELECT_CLASS}
            value={filter}
            disabled={disabled}
            onChange={(e) => onFilterChange(e.target.value as FilterKey)}
          >
            <option value="none">{t("cardChoice.bulk.filterNone")}</option>
            <option value="playable">{t("cardChoice.bulk.filterPlayable")}</option>
            <option value="creatures">{t("cardChoice.bulk.filterCreatures")}</option>
            <option value="lands">{t("cardChoice.bulk.filterLands")}</option>
            <option value="nonland">{t("cardChoice.bulk.filterNonland")}</option>
          </select>
        </label>
      )}
      {showQuery && query !== undefined && onQueryChange && (
        <label className="flex items-center gap-1">
          {t("cardChoice.bulk.searchLabel")}
          <input
            className={SELECT_CLASS}
            type="search"
            value={query}
            disabled={disabled}
            placeholder={t("cardChoice.bulk.searchPlaceholder")}
            aria-label={t("cardChoice.bulk.searchCardsAria")}
            onChange={(e) => onQueryChange(e.target.value)}
          />
        </label>
      )}
    </div>
  );
}
