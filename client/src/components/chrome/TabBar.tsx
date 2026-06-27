import { useTranslation } from "react-i18next";
import { Link, useLocation } from "react-router";

import { BuildBadge } from "./BuildBadge";
import { activeNavKey, NAV_ITEMS } from "./navItems";
import { SparkleIcon } from "./SparkleIcon";

interface TabBarProps {
  onWhatsNew: () => void;
  /** When true, an unread dot rides the "What's New" tab. */
  hasUnread: boolean;
}

/**
 * Mobile bottom tab bar (<820px) — the rail's counterpart. Same five primary
 * destinations plus the "What's New" affordance the rail carries; the rail is
 * hidden at this width.
 */
export function TabBar({ onWhatsNew, hasUnread }: TabBarProps) {
  const { t } = useTranslation("menu");
  const active = activeNavKey(useLocation().pathname);

  return (
    <nav
      className="fixed inset-x-0 bottom-0 z-50 flex items-stretch justify-around gap-0.5 border-t border-hairline-strong bg-[rgba(6,10,22,0.92)] px-1.5 pt-2 pb-[calc(0.5rem+env(safe-area-inset-bottom))] backdrop-blur-xl min-[820px]:hidden"
      aria-label={t("nav.label")}
    >
      {/* Version/update chip, anchored just above the bar's real top edge
          (`bottom-full`) — no magic viewport offset to keep in sync with the
          bar's height. Absolutely positioned so it stays out of the nav's flex
          flow. */}
      <BuildBadge inline className="absolute bottom-full left-2 mb-2" />
      {NAV_ITEMS.map(({ key, path, labelKey, Icon }) => {
        const on = active === key;
        return (
          <Link
            key={key}
            to={path}
            aria-current={on ? "page" : undefined}
            className={`flex flex-1 flex-col items-center gap-1 rounded-xl px-0.5 py-1.5 text-[10.5px] font-semibold transition-colors ${
              on ? "text-white" : "text-fg-meta"
            }`}
          >
            <Icon
              className={`h-7 w-7 transition-opacity ${on ? "opacity-100" : "opacity-50"}`}
            />
            <span>{t(labelKey)}</span>
          </Link>
        );
      })}

      <button
        onClick={onWhatsNew}
        className="flex flex-1 flex-col items-center gap-1 rounded-xl px-0.5 py-1.5 text-[10.5px] font-semibold text-fg-meta transition-colors"
      >
        <span className="relative">
          <SparkleIcon className="h-7 w-7 opacity-50" />
          {hasUnread && (
            <span className="absolute -right-1 -top-0.5 h-2 w-2 rounded-full bg-amber-400 ring-2 ring-[rgba(6,10,22,0.92)]">
              <span className="sr-only">{t("whatsNew.unread")}</span>
            </span>
          )}
        </span>
        <span>{t("nav.whatsNew")}</span>
      </button>
    </nav>
  );
}
