import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

interface DeckCardContextMenuProps {
  x: number;
  y: number;
  cardName: string;
  hasOverride: boolean;
  hasAlternates: boolean;
  onChooseArt: () => void;
  onClearOverride: () => void;
  onClose: () => void;
}

export function DeckCardContextMenu({
  x,
  y,
  cardName,
  hasOverride,
  hasAlternates,
  onChooseArt,
  onClearOverride,
  onClose,
}: DeckCardContextMenuProps) {
  const { t } = useTranslation("deck-builder");
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
      <div className="border-b border-white/8 px-3 py-1.5 text-xs text-slate-400 truncate">
        {cardName}
      </div>
      {hasAlternates && (
        <button
          role="menuitem"
          type="button"
          onClick={() => {
            onChooseArt();
            onClose();
          }}
          className="flex w-full items-center px-3 py-2 text-left text-sm text-gray-200 transition-colors hover:bg-white/10"
        >
          {t("contextMenu.chooseArt")}
        </button>
      )}
      {!hasAlternates && !hasOverride && (
        <div className="px-3 py-2 text-sm text-slate-500 italic">
          {t("contextMenu.noAlternates")}
        </div>
      )}
      {hasOverride && (
        <button
          role="menuitem"
          type="button"
          onClick={() => {
            onClearOverride();
            onClose();
          }}
          className="flex w-full items-center px-3 py-2 text-left text-sm text-gray-200 transition-colors hover:bg-white/10"
        >
          {t("contextMenu.clearOverride")}
        </button>
      )}
    </div>
  );
}
