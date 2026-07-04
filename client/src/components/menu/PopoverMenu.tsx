import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

const MENU_GAP_PX = 4;
const MENU_VIEWPORT_PADDING_PX = 8;
const OPEN_UP_THRESHOLD_PX = 200;

interface PopoverMenuStyle {
  top: number | "auto";
  bottom: number | "auto";
  left: number;
  maxHeight: number;
}

interface PopoverMenuProps {
  ariaLabel: string;
  /** Menu panel width in px — the menu is wider than the kebab trigger. */
  menuWidthPx?: number;
  /** Extra classes on the kebab trigger button. */
  triggerClassName?: string;
  /** Render the menu items; call `close` after an action runs. */
  children: (close: () => void) => ReactNode;
}

/**
 * Kebab (⋯) trigger + portaled `role="menu"` popover with outside-click /
 * Escape dismissal and viewport-aware placement (flips above when it would
 * clip below). Shared by the per-deck and per-folder action menus so there is
 * one popover authority — `MenuSelect` is a single-select `listbox` whose menu
 * tracks its trigger width, which doesn't fit an icon-triggered action menu.
 */
export function PopoverMenu({
  ariaLabel,
  menuWidthPx = 224,
  triggerClassName,
  children,
}: PopoverMenuProps) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const [style, setStyle] = useState<PopoverMenuStyle>({
    top: 0,
    bottom: "auto",
    left: 0,
    maxHeight: 320,
  });

  const close = useCallback(() => setOpen(false), []);

  const position = useCallback(() => {
    const trigger = triggerRef.current;
    if (!trigger) return;
    const rect = trigger.getBoundingClientRect();
    const left = Math.max(
      MENU_VIEWPORT_PADDING_PX,
      Math.min(
        rect.right - menuWidthPx,
        window.innerWidth - menuWidthPx - MENU_VIEWPORT_PADDING_PX,
      ),
    );
    const spaceBelow = window.innerHeight - rect.bottom - MENU_GAP_PX - MENU_VIEWPORT_PADDING_PX;
    const spaceAbove = rect.top - MENU_GAP_PX - MENU_VIEWPORT_PADDING_PX;
    const openUp = spaceBelow < OPEN_UP_THRESHOLD_PX && spaceAbove > spaceBelow;
    setStyle({
      top: openUp ? "auto" : rect.bottom + MENU_GAP_PX,
      bottom: openUp ? window.innerHeight - rect.top + MENU_GAP_PX : "auto",
      left,
      maxHeight: Math.max(120, openUp ? spaceAbove : spaceBelow),
    });
  }, [menuWidthPx]);

  useLayoutEffect(() => {
    if (open) position();
  }, [open, position]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (triggerRef.current?.contains(target) || menuRef.current?.contains(target)) return;
      close();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        close();
        triggerRef.current?.focus();
      }
    };
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("resize", position);
    window.addEventListener("scroll", position, true);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("resize", position);
      window.removeEventListener("scroll", position, true);
    };
  }, [open, close, position]);

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={ariaLabel}
        onClick={(event) => {
          event.stopPropagation();
          setOpen((prev) => !prev);
        }}
        className={
          triggerClassName ??
          "flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-black/35 text-gray-400 transition-colors hover:bg-white/15 hover:text-white"
        }
      >
        <svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true" className="h-4 w-4">
          <path d="M8 4a1.25 1.25 0 1 0 0-2.5A1.25 1.25 0 0 0 8 4Zm0 5.25a1.25 1.25 0 1 0 0-2.5 1.25 1.25 0 0 0 0 2.5ZM9.25 13.25a1.25 1.25 0 1 1-2.5 0 1.25 1.25 0 0 1 2.5 0Z" />
        </svg>
      </button>

      {open &&
        createPortal(
          <div
            ref={menuRef}
            role="menu"
            aria-label={ariaLabel}
            onClick={(event) => event.stopPropagation()}
            style={{
              top: style.top,
              bottom: style.bottom,
              left: style.left,
              width: menuWidthPx,
              maxHeight: style.maxHeight,
            }}
            className="fixed z-[120] flex flex-col overflow-y-auto overscroll-contain rounded-xl border border-white/10 bg-[#0a0f1b]/98 py-1 shadow-xl backdrop-blur-md thin-scrollbar"
          >
            {children(close)}
          </div>,
          document.body,
        )}
    </>
  );
}

/** Shared menu-item button styling for {@link PopoverMenu} children. */
export const popoverMenuItemClass =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-slate-200 transition-colors hover:bg-white/10 focus-visible:bg-white/10 focus-visible:outline-none";
