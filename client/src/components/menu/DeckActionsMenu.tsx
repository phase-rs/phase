import { useTranslation } from "react-i18next";

import type { DeckFolder } from "../../constants/storage";
import { PopoverMenu, popoverMenuItemClass } from "./PopoverMenu";

interface DeckActionsMenuProps {
  starred: boolean;
  folders: DeckFolder[];
  /** Folder the deck currently lives in (checked, omitted as a move target). */
  currentFolderId?: string;
  onToggleStar: () => void;
  /** Assign to a folder, or `null` for Unfiled. */
  onAssignFolder: (folderId: string | null) => void;
  /** Create a new folder and move this deck into it (parent prompts for a name). */
  onNewFolder: () => void;
}

/**
 * Per-tile overflow (kebab) menu for organizing a saved user deck: star/unstar
 * and move-to-folder (including "New folder…"). Built on {@link PopoverMenu}.
 */
export function DeckActionsMenu({
  starred,
  folders,
  currentFolderId,
  onToggleStar,
  onAssignFolder,
  onNewFolder,
}: DeckActionsMenuProps) {
  const { t } = useTranslation("menu");

  const runAndClose = (close: () => void, action: () => void) => (event: React.MouseEvent) => {
    event.stopPropagation();
    action();
    close();
  };

  return (
    <PopoverMenu ariaLabel={t("deckTile.actionsMenu")}>
      {(close) => (
        <>
          <button
            type="button"
            role="menuitem"
            onClick={runAndClose(close, onToggleStar)}
            className={popoverMenuItemClass}
          >
            <StarIcon filled={starred} />
            {starred ? t("deckTile.unstar") : t("deckTile.star")}
          </button>

          <div className="my-1 border-t border-white/8" />
          <div
            role="presentation"
            className="px-3 py-1 text-[0.62rem] font-semibold uppercase tracking-[0.18em] text-slate-500"
          >
            {t("deckTile.moveToFolder")}
          </div>

          <button
            type="button"
            role="menuitemradio"
            aria-checked={currentFolderId == null}
            onClick={runAndClose(close, () => onAssignFolder(null))}
            className={popoverMenuItemClass}
          >
            <span className="min-w-0 flex-1 truncate">{t("deckTile.unfiled")}</span>
            {currentFolderId == null && <CheckIcon />}
          </button>
          {folders.map((folder) => (
            <button
              key={folder.id}
              type="button"
              role="menuitemradio"
              aria-checked={currentFolderId === folder.id}
              onClick={runAndClose(close, () => onAssignFolder(folder.id))}
              className={popoverMenuItemClass}
              title={folder.name}
            >
              <span className="min-w-0 flex-1 truncate">{folder.name}</span>
              {currentFolderId === folder.id && <CheckIcon />}
            </button>
          ))}
          <button
            type="button"
            role="menuitem"
            onClick={runAndClose(close, onNewFolder)}
            className={popoverMenuItemClass}
          >
            <svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true" className="h-3.5 w-3.5 text-slate-400">
              <path d="M8.75 4.75a.75.75 0 0 0-1.5 0V7.25H4.75a.75.75 0 0 0 0 1.5H7.25v2.5a.75.75 0 0 0 1.5 0V8.75h2.5a.75.75 0 0 0 0-1.5H8.75V4.75Z" />
            </svg>
            {t("deckTile.newFolder")}
          </button>
        </>
      )}
    </PopoverMenu>
  );
}

function StarIcon({ filled }: { filled: boolean }) {
  return (
    <svg
      viewBox="0 0 16 16"
      fill={filled ? "currentColor" : "none"}
      stroke="currentColor"
      strokeWidth={filled ? 0 : 1.3}
      aria-hidden="true"
      className={`h-3.5 w-3.5 ${filled ? "text-amber-300" : "text-slate-400"}`}
    >
      <path d="M8 1.5l1.9 3.85 4.25.62-3.07 3 .72 4.23L8 11.2l-3.8 2 .72-4.23-3.07-3 4.25-.62L8 1.5Z" />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true" className="h-3.5 w-3.5 shrink-0 text-emerald-300">
      <path d="M13.78 4.22a.75.75 0 0 1 0 1.06l-6.5 6.5a.75.75 0 0 1-1.06 0l-3-3a.75.75 0 1 1 1.06-1.06L6.75 10.19l5.97-5.97a.75.75 0 0 1 1.06 0Z" />
    </svg>
  );
}
