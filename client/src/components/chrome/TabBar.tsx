import { useTranslation } from "react-i18next";
import { Link, useLocation } from "react-router";

import { activeNavKey, NAV_ITEMS } from "./navItems";

/**
 * Mobile bottom tab bar (<820px) — the rail's counterpart. Same five primary
 * destinations; the rail is hidden at this width.
 */
export function TabBar() {
  const { t } = useTranslation("menu");
  const active = activeNavKey(useLocation().pathname);

  return (
    <nav
      className="fixed inset-x-0 bottom-0 z-50 flex items-stretch justify-around gap-0.5 border-t border-hairline-strong bg-[rgba(6,10,22,0.92)] px-1.5 pt-2 pb-[calc(0.5rem+env(safe-area-inset-bottom))] backdrop-blur-xl min-[820px]:hidden"
      aria-label={t("nav.label")}
    >
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
    </nav>
  );
}
