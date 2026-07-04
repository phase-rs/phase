import type { CSSProperties } from "react";
import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";

const MENU_GAP_PX = 4;
const MENU_VIEWPORT_PADDING_PX = 8;
const MENU_MAX_HEIGHT_PX = 280;
/** Matches AppShell `pb-[76px]` tab-bar reserve on viewports below 820px. */
const MOBILE_TAB_BAR_RESERVE_PX = 76;
/** Shell tab bar appears below this width — menus become bottom sheets. */
const MOBILE_SHEET_MEDIA_QUERY = "(max-width: 819px)";

function useMobileSheetLayout(): boolean {
  const [mobileSheet, setMobileSheet] = useState(() =>
    typeof window !== "undefined"
      ? window.matchMedia(MOBILE_SHEET_MEDIA_QUERY).matches
      : false,
  );

  useEffect(() => {
    const mediaQuery = window.matchMedia(MOBILE_SHEET_MEDIA_QUERY);
    const handleChange = () => setMobileSheet(mediaQuery.matches);
    mediaQuery.addEventListener("change", handleChange);
    return () => mediaQuery.removeEventListener("change", handleChange);
  }, []);

  return mobileSheet;
}

function getViewportBottomInset(): number {
  if (window.matchMedia("(min-width: 820px)").matches) {
    return MENU_VIEWPORT_PADDING_PX;
  }
  return MOBILE_TAB_BAR_RESERVE_PX + MENU_VIEWPORT_PADDING_PX;
}

function getViewportBottom(): number {
  const visualViewport = window.visualViewport;
  if (visualViewport) {
    return visualViewport.offsetTop + visualViewport.height - getViewportBottomInset();
  }
  return window.innerHeight - getViewportBottomInset();
}

function getViewportTop(): number {
  return window.visualViewport?.offsetTop ?? 0;
}
// Cap how far the widest item label can grow the closed trigger — a single
// long deck name must not stretch the toolbar (items truncate with a title
// tooltip beyond this).
const TRIGGER_MAX_WIDTH_PX = 320;

export interface MenuSelectItem {
  value: string;
  label: string;
}

export interface MenuSelectGroup {
  label: string;
  items: MenuSelectItem[];
}

export interface MenuSelectProps {
  /** Visible trigger label (e.g. placeholder text). */
  label: string;
  /** Ungrouped options rendered before any `groups` sections. */
  items?: MenuSelectItem[];
  /** Grouped sections (e.g. Constructed / Commander format families). */
  groups?: MenuSelectGroup[];
  onSelect: (value: string) => void;
  disabled?: boolean;
  /** Accessible name when `label` shows the current value instead of the control name. */
  ariaLabel?: string;
  /** Highlights the matching option in the open menu. */
  selectedValue?: string;
  /**
   * When true, render a search box at the top of the open menu that filters
   * items/groups by label substring. Focus lands on the input when the menu
   * opens (instead of the first option). Off by default — existing call sites
   * keep their plain listbox behavior.
   */
  filterable?: boolean;
  /** Placeholder for the filter input (only used when `filterable`). */
  filterPlaceholder?: string;
  /** Message shown when a `filterable` search matches no options. */
  noMatchesLabel?: string;
  /**
   * `auto` (default): bottom sheet below 820px, anchored dropdown at wider
   * widths. `dropdown`: always anchor below/above the trigger like a native
   * select — use inside scrollable panels (e.g. deck-builder filters).
   */
  menuLayout?: "auto" | "dropdown";
  /**
   * When true, the trigger fills its wrapper and truncates long labels instead
   * of growing the wrapper to fit the widest option. Use on full-width form
   * fields; omit in compact toolbars where the trigger should size to content.
   */
  fitContainer?: boolean;
  /** Class on the outer relative wrapper (e.g. `max-w-[8rem] shrink-0`). */
  wrapperClassName?: string;
  /** Class on the trigger button. */
  className?: string;
  /** Inline style on the trigger button (e.g. seat-color tint). */
  triggerStyle?: CSSProperties;
  /** Class on the chevron icon wrapper. */
  chevronClassName?: string;
  /** Class on the portaled menu panel. */
  menuClassName?: string;
  /** z-index class for the portaled menu (default `z-[120]`). Raise inside high-z overlays like the debug panel. */
  menuZClassName?: string;
  /** z-index class for the mobile bottom-sheet backdrop. */
  backdropZClassName?: string;
  /** Per-option inline style (e.g. seat-color labels). */
  getOptionStyle?: (item: MenuSelectItem) => CSSProperties | undefined;
  /** Fired when the pointer or focus moves over an option, or leaves the menu. */
  onOptionHover?: (value: string | null) => void;
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

function flattenMenuItems(
  items: MenuSelectItem[] | undefined,
  groups: MenuSelectGroup[] | undefined,
): MenuSelectItem[] {
  return [
    ...(items ?? []),
    ...(groups ?? []).flatMap((group) => group.items),
  ];
}

type AnchoredMenuStyle = {
  top: number | "auto";
  bottom: number | "auto";
  left: number;
  width: number;
  maxHeight: number;
  boxSizing: "border-box";
};

function computeAnchoredMenuStyle(trigger: HTMLElement): AnchoredMenuStyle {
  const rect = trigger.getBoundingClientRect();
  const viewport = window.visualViewport;
  const viewportLeft = viewport?.offsetLeft ?? 0;
  const viewportWidth = viewport?.width ?? window.innerWidth;
  const viewportTop = getViewportTop();
  const viewportBottom = getViewportBottom();

  // Pin the menu to the trigger's box; only nudge when the menu would clip.
  let width = rect.width;
  let left = rect.left;
  const minLeft = viewportLeft + MENU_VIEWPORT_PADDING_PX;
  const maxRight = viewportLeft + viewportWidth - MENU_VIEWPORT_PADDING_PX;

  if (left + width > maxRight) {
    left = Math.max(minLeft, maxRight - width);
  }
  if (left < minLeft) {
    left = minLeft;
    width = Math.min(width, maxRight - minLeft);
  }
  const spaceBelow = Math.max(0, viewportBottom - rect.bottom - MENU_GAP_PX);
  const spaceAbove = Math.max(0, rect.top - viewportTop - MENU_GAP_PX);
  const openUp = spaceBelow < MENU_MAX_HEIGHT_PX && spaceAbove > spaceBelow;
  const maxHeight = Math.min(MENU_MAX_HEIGHT_PX, openUp ? spaceAbove : spaceBelow);

  return {
    left: Math.round(left),
    width: Math.round(width),
    maxHeight: Math.max(maxHeight, 0),
    top: openUp ? "auto" : Math.round(rect.bottom + MENU_GAP_PX),
    bottom: openUp ? Math.round(window.innerHeight - rect.top + MENU_GAP_PX) : "auto",
    boxSizing: "border-box",
  };
}

export function MenuSelect({
  label,
  items,
  groups,
  onSelect,
  disabled = false,
  ariaLabel,
  selectedValue,
  filterable = false,
  filterPlaceholder,
  noMatchesLabel,
  menuLayout = "auto",
  fitContainer = false,
  wrapperClassName = "",
  className = "",
  triggerStyle,
  chevronClassName = "text-white/70",
  menuClassName = "",
  menuZClassName = "z-[120]",
  backdropZClassName = "z-[119]",
  getOptionStyle,
  onOptionHover,
}: MenuSelectProps) {
  const listboxId = useId();
  const mobileSheet = useMobileSheetLayout();
  const useBottomSheet = menuLayout === "auto" && mobileSheet;
  const [open, setOpen] = useState(false);
  const [filterText, setFilterText] = useState("");
  const [minWidthPx, setMinWidthPx] = useState<number | undefined>(undefined);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const measureRef = useRef<HTMLSpanElement>(null);
  const filterInputRef = useRef<HTMLInputElement>(null);
  const allItems = useMemo(() => flattenMenuItems(items, groups), [items, groups]);

  // When filterable, narrow items/groups by label substring; empty groups drop.
  const filterQuery = filterable ? filterText.trim().toLowerCase() : "";
  const visibleItems = useMemo(
    () =>
      filterQuery
        ? (items ?? []).filter((item) => item.label.toLowerCase().includes(filterQuery))
        : items,
    [items, filterQuery],
  );
  const visibleGroups = useMemo(
    () =>
      filterQuery
        ? (groups ?? [])
            .map((group) => ({
              ...group,
              items: group.items.filter((item) =>
                item.label.toLowerCase().includes(filterQuery),
              ),
            }))
            .filter((group) => group.items.length > 0)
        : groups,
    [groups, filterQuery],
  );
  const hasVisibleOptions =
    (visibleItems?.length ?? 0) > 0 || (visibleGroups?.length ?? 0) > 0;
  const [menuStyle, setMenuStyle] = useState<AnchoredMenuStyle>({
    top: 0,
    bottom: "auto",
    left: 0,
    width: 0,
    maxHeight: MENU_MAX_HEIGHT_PX,
    boxSizing: "border-box",
  });

  useLayoutEffect(() => {
    if (fitContainer) {
      setMinWidthPx(undefined);
      return;
    }

    const measure = measureRef.current;
    if (!measure) return;

    let contentWidth = 0;
    for (const sample of [label, ...allItems.map((item) => item.label)]) {
      measure.textContent = sample;
      contentWidth = Math.max(contentWidth, measure.offsetWidth);
    }

    // px-3 padding + chevron + gap between label and icon.
    setMinWidthPx(Math.min(contentWidth + 48, TRIGGER_MAX_WIDTH_PX));
  }, [fitContainer, label, allItems]);

  const updatePosition = useCallback(() => {
    if (useBottomSheet) return;
    const trigger = triggerRef.current;
    if (!trigger) return;
    setMenuStyle(computeAnchoredMenuStyle(trigger));
  }, [useBottomSheet]);

  const onOptionHoverRef = useRef(onOptionHover);
  onOptionHoverRef.current = onOptionHover;

  const closeMenu = useCallback(() => {
    setOpen(false);
    setFilterText("");
    onOptionHoverRef.current?.(null);
  }, []);

  const toggleOpen = useCallback(() => {
    if (disabled) return;
    if (open) {
      closeMenu();
      return;
    }
    const trigger = triggerRef.current;
    if (trigger && !useBottomSheet) {
      setMenuStyle(computeAnchoredMenuStyle(trigger));
    }
    setOpen(true);
  }, [closeMenu, disabled, open, useBottomSheet]);

  useLayoutEffect(() => {
    if (!open) return;
    updatePosition();
    // When filterable, focus the search box so the user can type-to-narrow
    // immediately; ArrowDown then drops into the option list.
    if (filterable) {
      filterInputRef.current?.focus();
      return;
    }
    // APG listbox pattern: move focus into the menu so the keyboard path
    // (Arrow keys + Enter) works like the native select this replaces.
    const menu = menuRef.current;
    const selectedOption =
      selectedValue != null
        ? menu?.querySelector<HTMLButtonElement>(`[role="option"][aria-selected="true"]`)
        : null;
    (selectedOption ?? menu?.querySelector<HTMLButtonElement>('[role="option"]'))?.focus();
    selectedOption?.scrollIntoView({ block: "nearest" });
  }, [open, selectedValue, updatePosition, useBottomSheet, filterable]);

  useEffect(() => {
    if (!open) return;

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (triggerRef.current?.contains(target) || menuRef.current?.contains(target)) return;
      closeMenu();
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        closeMenu();
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
    window.visualViewport?.addEventListener("resize", updatePosition);
    window.visualViewport?.addEventListener("scroll", updatePosition);
    scrollParents.forEach((parent) => {
      parent.addEventListener("scroll", handleScroll, { passive: true });
    });

    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("resize", updatePosition);
      window.visualViewport?.removeEventListener("resize", updatePosition);
      window.visualViewport?.removeEventListener("scroll", updatePosition);
      scrollParents.forEach((parent) => {
        parent.removeEventListener("scroll", handleScroll);
      });
    };
  }, [closeMenu, open, updatePosition, useBottomSheet]);

  const renderOption = (item: MenuSelectItem) => (
    <button
      key={item.value}
      type="button"
      role="option"
      onClick={() => {
        onSelect(item.value);
        closeMenu();
      }}
      onMouseEnter={() => onOptionHoverRef.current?.(item.value)}
      onMouseLeave={() => onOptionHoverRef.current?.(null)}
      onFocus={() => onOptionHoverRef.current?.(item.value)}
      onBlur={() => onOptionHoverRef.current?.(null)}
      aria-selected={selectedValue === item.value}
      style={getOptionStyle?.(item)}
      className={[
        "flex w-full min-w-0 items-center px-3 py-2 text-left text-sm transition-colors hover:bg-white/10 focus-visible:bg-white/10 focus-visible:outline-none",
        selectedValue === item.value ? "bg-white/10 text-white" : "text-slate-200",
      ].join(" ")}
      title={item.label}
    >
      <span className="min-w-0 truncate">{item.label}</span>
    </button>
  );

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
        aria-label={ariaLabel ?? label}
        onClick={toggleOpen}
        className={triggerClassName}
        style={triggerStyle}
      >
        <span className="min-w-0 truncate" title={label}>
          {label}
        </span>
        <ChevronDownIcon className={`h-4 w-4 shrink-0 ${chevronClassName}`} />
      </button>

      {open &&
        createPortal(
          <>
            {useBottomSheet && (
              <button
                type="button"
                aria-label={ariaLabel ?? label}
                className={`fixed inset-0 ${backdropZClassName} bg-black/60`}
                onClick={closeMenu}
              />
            )}
            <div
              ref={menuRef}
              id={listboxId}
              role="listbox"
              aria-label={ariaLabel ?? label}
              className={[
                `fixed ${menuZClassName} flex flex-col overflow-x-hidden overflow-y-auto overscroll-contain border border-white/10 bg-[#0a0f1b]/98 py-1 shadow-xl backdrop-blur-md thin-scrollbar`,
                useBottomSheet
                  ? "inset-x-0 bottom-[calc(76px+env(safe-area-inset-bottom))] max-h-[min(70dvh,calc(100dvh-76px-env(safe-area-inset-bottom)-1rem))] rounded-t-2xl rounded-b-none border-b-0"
                  : "rounded-xl",
                menuClassName,
              ].join(" ")}
              onWheel={(event) => event.stopPropagation()}
              style={
                useBottomSheet
                  ? undefined
                  : {
                      top: menuStyle.top,
                      bottom: menuStyle.bottom,
                      left: menuStyle.left,
                      width: menuStyle.width,
                      maxHeight: menuStyle.maxHeight,
                      boxSizing: menuStyle.boxSizing,
                    }
              }
            >
              {filterable && (
                <div className="sticky top-0 z-10 border-b border-white/10 bg-[#0a0f1b] px-2 pb-1.5 pt-1.5">
                  <input
                    ref={filterInputRef}
                    type="text"
                    value={filterText}
                    onChange={(event) => setFilterText(event.target.value)}
                    placeholder={filterPlaceholder ?? ""}
                    aria-label={filterPlaceholder ?? ariaLabel ?? label}
                    className="w-full rounded-md bg-black/40 px-2 py-1.5 text-sm text-slate-200 outline-none ring-1 ring-white/10 placeholder:text-slate-500 focus:ring-white/25"
                  />
                </div>
              )}
              {visibleItems?.map((item) => renderOption(item))}
              {visibleGroups?.map((group) => (
                <div key={group.label}>
                  <div
                    role="presentation"
                    className="px-3 py-1.5 text-[0.62rem] font-semibold uppercase tracking-[0.18em] text-slate-500"
                  >
                    {group.label}
                  </div>
                  {group.items.map((item) => renderOption(item))}
                </div>
              ))}
              {filterable && !hasVisibleOptions && noMatchesLabel && (
                <div className="px-3 py-3 text-center text-xs text-slate-500">
                  {noMatchesLabel}
                </div>
              )}
            </div>
          </>,
          document.body,
        )}
    </div>
  );
}
