import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

interface BoardContextMenuProps {
  x: number;
  y: number;
  onClose: () => void;
  onChangeBackground: () => void;
  onCustomizeLayout: () => void;
  onToggleGameLog: () => void;
  onToggleDebugLog: () => void;
  /** Open the "Report a card problem" picker. Rendered only when provided
   *  (live, participating games). */
  onReportCard?: () => void;
}

export function BoardContextMenu({
  x,
  y,
  onClose,
  onChangeBackground,
  onCustomizeLayout,
  onToggleGameLog,
  onToggleDebugLog,
  onReportCard,
}: BoardContextMenuProps) {
  const { t } = useTranslation("game");
  const ref = useRef<HTMLDivElement | null>(null);
  const [position, setPosition] = useState({ left: x, top: y });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const maxLeft = window.innerWidth - rect.width - 8;
    const maxTop = window.innerHeight - rect.height - 8;
    setPosition({
      left: Math.max(8, Math.min(x, maxLeft)),
      top: Math.max(8, Math.min(y, maxTop)),
    });
  }, [x, y]);

  useEffect(() => {
    const handlePointerDown = (e: PointerEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("keydown", handleKey);
    window.addEventListener("blur", onClose);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKey);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("resize", onClose);
    };
  }, [onClose]);

  return (
    <div
      ref={ref}
      role="menu"
      className="fixed z-[110] w-56 rounded-lg border border-gray-700 bg-gray-900/95 py-1 shadow-xl backdrop-blur-sm"
      style={{ left: position.left, top: position.top }}
      onContextMenu={(e) => e.preventDefault()}
    >
      <MenuItem
        label={t("board.changeBackground")}
        onClick={() => {
          onChangeBackground();
          onClose();
        }}
      />
      <MenuItem
        label={t("board.customizeLayout")}
        shortcut="Ctrl+Shift+L"
        onClick={() => {
          onCustomizeLayout();
          onClose();
        }}
      />
      <MenuItem
        label={t("board.gameLog")}
        onClick={() => {
          onToggleGameLog();
          onClose();
        }}
      />
      <MenuItem
        label={t("board.debugLog")}
        shortcut="`"
        onClick={() => {
          onToggleDebugLog();
          onClose();
        }}
      />
      {onReportCard && (
        <MenuItem
          label={t("board.reportCard")}
          onClick={() => {
            onReportCard();
            onClose();
          }}
        />
      )}
    </div>
  );
}

function MenuItem({
  label,
  onClick,
  shortcut,
}: {
  label: string;
  onClick: () => void;
  shortcut?: string;
}) {
  return (
    <button
      role="menuitem"
      type="button"
      onClick={onClick}
      className="flex w-full items-center justify-between px-3 py-2 text-left text-sm text-gray-200 transition-colors hover:bg-white/10"
    >
      <span>{label}</span>
      {shortcut && (
        <span className="ml-3 font-mono text-xs text-gray-500">{shortcut}</span>
      )}
    </button>
  );
}
