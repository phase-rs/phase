import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

const MENU_GAP_PX = 4;
const MENU_MAX_HEIGHT_PX = 280;
// Cap how far the widest item label can grow the closed trigger — a single
// long deck name must not stretch the toolbar (items truncate with a title
// tooltip beyond this).
const TRIGGER_MAX_WIDTH_PX = 320;

export interface MenuSelectItem {
  value: string;
  label: string;
}

export interface MenuSelectProps {
  /** Visible trigger label (e.g. placeholder text). */
  label: string;
  items: MenuSelectItem[];
  onSelect: (value: string) => void;
  disabled?: boolean;
  /** Class on the outer relative wrapper (e.g. `max-w-[8rem] shrink-0`). */
  wrapperClassName?: string;
  /** Class on the trigger button. */
  className?: string;
}

function ChevronDownIcon({ className }: { className: string }) {
  return (
    <svg aria-hidden="true" viewBox="0 0 20 20" className={`fill-current ${className}`}>
      <path d="M5.47 7.97a.75.75 0 0 1 1.06 0L10 11.44l3.47-3.47a.75.75 0 1 1 1.06 1.06l-4 4a.75.75 0 0 1-1.06 0l-4-4a.75.75 0 0 1 0-1.06Z" />
    </svg>
  );
}

function getScrollParents(element: HTMLElement | null): (HTMLElement | Window)[] {
  const parents: (HTMLElement | Window)[] = [window];
  let node = element?.parentElement ?? null;

  while (node) {
    const { overflow, overflowY, overflowX } = getComputedStyle(node);
    const scrollable = [overflow, overflowY, overflowX].some(
      (value) => value === "auto" || value === "scroll" || value === "overlay",
    );
    if (scrollable) parents.push(node);
    node = node.parentElement;
  }

  return parents;
}

export function MenuSelect({
  label,
  items,
  onSelect,
  disabled = false,
  wrapperClassName = "",
  className = "",
}: MenuSelectProps) {
  const listboxId = useId();
  const [open, setOpen] = useState(false);
  const [minWidthPx, setMinWidthPx] = useState<number | undefined>(undefined);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const measureRef = useRef<HTMLSpanElement>(null);
  const itemsKey = items.map((item) => item.label).join("\0");
  const [menuStyle, setMenuStyle] = useState<{
    top: number | "auto";
    bottom: number | "auto";
    left: number;
    width: number;
    maxHeight: number;
  }>({
    top: 0,
    bottom: "auto",
    left: 0,
    width: 0,
    maxHeight: MENU_MAX_HEIGHT_PX,
  });

  useLayoutEffect(() => {
    const measure = measureRef.current;
    if (!measure) return;

    let contentWidth = 0;
    for (const sample of [label, ...items.map((item) => item.label)]) {
      measure.textContent = sample;
      contentWidth = Math.max(contentWidth, measure.offsetWidth);
    }

    // px-3 padding + chevron + gap between label and icon.
    setMinWidthPx(Math.min(contentWidth + 48, TRIGGER_MAX_WIDTH_PX));
  }, [label, itemsKey]);

  const updatePosition = useCallback(() => {
    const trigger = triggerRef.current;
    if (!trigger) return;

    const rect = trigger.getBoundingClientRect();
    const spaceBelow = Math.max(0, window.innerHeight - rect.bottom - MENU_GAP_PX);
    const spaceAbove = Math.max(0, rect.top - MENU_GAP_PX);
    const openUp = spaceBelow < MENU_MAX_HEIGHT_PX && spaceAbove > spaceBelow;
    const maxHeight = Math.min(MENU_MAX_HEIGHT_PX, openUp ? spaceAbove : spaceBelow);

    setMenuStyle({
      left: rect.left,
      width: rect.width,
      maxHeight: Math.max(maxHeight, 0),
      top: openUp ? "auto" : rect.bottom + MENU_GAP_PX,
      bottom: openUp ? window.innerHeight - rect.top + MENU_GAP_PX : "auto",
    });
  }, []);

  useLayoutEffect(() => {
    if (!open) return;
    updatePosition();
    // APG listbox pattern: move focus into the menu so the keyboard path
    // (Arrow keys + Enter) works like the native select this replaces.
    menuRef.current?.querySelector<HTMLButtonElement>('[role="option"]')?.focus();
  }, [open, updatePosition]);

  useEffect(() => {
    if (!open) return;

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (triggerRef.current?.contains(target) || menuRef.current?.contains(target)) return;
      setOpen(false);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpen(false);
        triggerRef.current?.focus();
        return;
      }
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      const options = menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="option"]');
      if (!options || options.length === 0) return;
      event.preventDefault();
      const current = Array.prototype.indexOf.call(options, document.activeElement);
      const next =
        current < 0
          ? event.key === "ArrowDown"
            ? 0
            : options.length - 1
          : (current + (event.key === "ArrowDown" ? 1 : -1) + options.length) % options.length;
      options[next].focus();
    };
    const handleScroll = (event: Event) => {
      const target = event.target as Node | null;
      if (menuRef.current && target && menuRef.current.contains(target)) return;
      updatePosition();
    };

    const scrollParents = getScrollParents(triggerRef.current);

    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("resize", updatePosition);
    scrollParents.forEach((parent) => {
      parent.addEventListener("scroll", handleScroll, { passive: true });
    });

    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("resize", updatePosition);
      scrollParents.forEach((parent) => {
        parent.removeEventListener("scroll", handleScroll);
      });
    };
  }, [open, updatePosition]);

  const triggerClassName = [
    "flex w-full items-center justify-between gap-2 rounded-xl border border-white/10 bg-black/18 px-3 py-1.5 text-left text-sm text-white transition-colors",
    "hover:bg-white/6 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/20",
    disabled ? "cursor-not-allowed opacity-40" : "",
    className,
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div
      className={`relative ${wrapperClassName}`.trim()}
      style={minWidthPx ? { minWidth: minWidthPx } : undefined}
    >
      <span
        ref={measureRef}
        aria-hidden="true"
        className="pointer-events-none invisible absolute text-sm whitespace-nowrap"
      />
      <button
        ref={triggerRef}
        type="button"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listboxId : undefined}
        aria-label={label}
        onClick={() => {
          if (disabled) return;
          setOpen((prev) => !prev);
        }}
        className={triggerClassName}
      >
        <span className="truncate">{label}</span>
        <ChevronDownIcon className="h-4 w-4 shrink-0 text-white/70" />
      </button>

      {open &&
        createPortal(
          <div
            ref={menuRef}
            id={listboxId}
            role="listbox"
            aria-label={label}
            className="fixed z-[120] flex flex-col overflow-x-hidden overflow-y-auto overscroll-contain rounded-xl border border-white/10 bg-[#0a0f1b]/98 py-1 shadow-xl backdrop-blur-md thin-scrollbar"
            onWheel={(event) => event.stopPropagation()}
            style={{
              top: menuStyle.top,
              bottom: menuStyle.bottom,
              left: menuStyle.left,
              width: menuStyle.width,
              maxHeight: menuStyle.maxHeight,
            }}
          >
            {items.map((item) => (
              <button
                key={item.value}
                type="button"
                role="option"
                onClick={() => {
                  onSelect(item.value);
                  setOpen(false);
                }}
                className="flex w-full min-w-0 items-center px-3 py-2 text-left text-sm text-slate-200 transition-colors hover:bg-white/10 focus-visible:bg-white/10 focus-visible:outline-none"
                title={item.label}
              >
                <span className="min-w-0 truncate">{item.label}</span>
              </button>
            ))}
          </div>,
          document.body,
        )}
    </div>
  );
}
