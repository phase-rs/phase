import { useTranslation } from "react-i18next";

import { PopoverMenu, popoverMenuItemClass } from "./PopoverMenu";

interface FolderActionsMenuProps {
  onRename: () => void;
  onDelete: () => void;
}

/** Per-folder header kebab: rename or delete a user folder (its decks move to
 * Unfiled on delete — handled by the caller). Built on {@link PopoverMenu}. */
export function FolderActionsMenu({ onRename, onDelete }: FolderActionsMenuProps) {
  const { t } = useTranslation("menu");

  const runAndClose = (close: () => void, action: () => void) => (event: React.MouseEvent) => {
    event.stopPropagation();
    action();
    close();
  };

  return (
    <PopoverMenu ariaLabel={t("folder.actionsMenu")} menuWidthPx={176}>
      {(close) => (
        <>
          <button
            type="button"
            role="menuitem"
            onClick={runAndClose(close, onRename)}
            className={popoverMenuItemClass}
          >
            {t("folder.rename")}
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={runAndClose(close, onDelete)}
            className={`${popoverMenuItemClass} text-red-300 hover:bg-red-500/15`}
          >
            {t("folder.delete")}
          </button>
        </>
      )}
    </PopoverMenu>
  );
}
