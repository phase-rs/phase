import { useRef } from "react";
import { useTranslation } from "react-i18next";
import { panelId, tabId, type DeckBuilderSurface } from "./deckBuilderTabs";

interface DeckBuilderTabBarProps {
  activeSurface: DeckBuilderSurface;
  onSurfaceChange: (surface: DeckBuilderSurface) => void;
  deckCount: number;
}

const TABS: { id: DeckBuilderSurface; labelKey: string }[] = [
  { id: "deck", labelKey: "tabs.deck" },
  { id: "info", labelKey: "tabs.info" },
];

export function DeckBuilderTabBar({
  activeSurface,
  onSurfaceChange,
  deckCount,
}: DeckBuilderTabBarProps) {
  const { t } = useTranslation("deck-builder");
  const tabRefs = useRef<(HTMLButtonElement | null)[]>([]);

  // APG tablist keyboard model with automatic activation: arrow keys move both
  // selection and focus (switching surfaces is cheap — panels just toggle
  // visibility — so focus-follows-selection is the right pattern). Home/End
  // jump to the ends.
  const handleKeyDown = (e: React.KeyboardEvent, index: number) => {
    let next = index;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") next = (index + 1) % TABS.length;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") next = (index - 1 + TABS.length) % TABS.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = TABS.length - 1;
    else return;
    e.preventDefault();
    onSurfaceChange(TABS[next].id);
    tabRefs.current[next]?.focus();
  };

  return (
    <div
      role="tablist"
      aria-label={t("tabs.ariaLabel")}
      className="flex shrink-0 gap-1 border-b border-white/8 bg-black/12 px-2 py-1.5 md:hidden"
    >
      {TABS.map((tab, index) => {
        const isActive = tab.id === activeSurface;
        return (
          <button
            key={tab.id}
            ref={(el) => {
              tabRefs.current[index] = el;
            }}
            type="button"
            role="tab"
            id={tabId(tab.id)}
            aria-selected={isActive}
            aria-controls={panelId(tab.id)}
            // Roving tabindex: only the active tab is in the tab sequence; the
            // rest are reached via arrow keys (APG tablist requirement).
            tabIndex={isActive ? 0 : -1}
            onClick={() => onSurfaceChange(tab.id)}
            onKeyDown={(e) => handleKeyDown(e, index)}
            className={`flex flex-1 items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-sm font-medium transition-colors ${
              isActive
                ? "bg-white/14 text-white"
                : "text-slate-400 hover:bg-white/6 hover:text-slate-200"
            }`}
          >
            {t(tab.labelKey)}
            {tab.id === "deck" && deckCount > 0 && (
              <span className="rounded-full bg-white/14 px-1.5 text-[0.7rem] tabular-nums text-slate-200">
                {deckCount}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}
