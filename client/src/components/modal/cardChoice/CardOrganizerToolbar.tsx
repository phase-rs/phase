import { useTranslation } from "react-i18next";

import type { FilterKey, GroupKey, SortKey } from "./gridSelection.ts";

export interface CardOrganizerToolbarProps {
  sort: SortKey;
  onSortChange: (sort: SortKey) => void;
  group?: GroupKey;
  onGroupChange?: (group: GroupKey) => void;
  filter?: FilterKey;
  onFilterChange?: (filter: FilterKey) => void;
  /** Which controls to render. Sort defaults on; group/filter default off so a
   *  caller opts into each axis it actually wants (the grid shows sort+group,
   *  the hand shows sort+filter). */
  showSort?: boolean;
  showGroup?: boolean;
  showFilter?: boolean;
  /** Disable every control (e.g. during target selection, when reorganizing the
   *  displayed order would be unsafe). */
  disabled?: boolean;
  className?: string;
}

const SELECT_CLASS =
  "rounded bg-black/40 px-1 py-0.5 disabled:cursor-not-allowed disabled:opacity-40";

/**
 * Presentational sort / group / filter selects for {@link useCardOrganizer}.
 * Stateless — it renders the current axis values and reports changes; the hook
 * (or the consuming component) owns the state. Shared by the card-choice grid
 * and the player's hand so both expose ONE organizing mechanism.
 */
export function CardOrganizerToolbar({
  sort,
  onSortChange,
  group,
  onGroupChange,
  filter,
  onFilterChange,
  showSort = true,
  showGroup = false,
  showFilter = false,
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
    </div>
  );
}
