import { useTranslation } from "react-i18next";
import { Link, useLocation, useNavigate } from "react-router";

import { BuildBadge } from "./BuildBadge";
import { activeNavKey, NAV_ITEMS } from "./navItems";
import { SOCIAL_LINKS, social } from "./socialLinks";

/**
 * Desktop navigation rail (≥820px). Logo → Home, the five primary destinations,
 * and a footer with Settings, labelled social badges, and the build/version
 * chip. Hidden below 820px, where TabBar + MobileSocialBar take over.
 */
interface RailProps {
  onSettings: () => void;
}

export function Rail({ onSettings }: RailProps) {
  const { t } = useTranslation("menu");
  const navigate = useNavigate();
  const active = activeNavKey(useLocation().pathname);

  return (
    <nav
      className="fixed inset-y-0 left-0 z-50 hidden w-[92px] flex-col items-center gap-2 border-r border-hairline-strong bg-[rgba(6,10,22,0.72)] px-2 py-[18px] backdrop-blur-xl min-[820px]:flex"
      aria-label={t("nav.label")}
    >
      <button
        onClick={() => navigate("/")}
        className="mb-2.5 cursor-pointer border-0 bg-transparent p-0"
        aria-label={t("nav.home")}
      >
        <img
          src="/logo_only.webp"
          alt="phase.rs"
          className="w-11 drop-shadow-[0_0_14px_rgba(251,146,60,0.45)]"
        />
      </button>

      <div className="flex w-full flex-col gap-1">
        {NAV_ITEMS.map(({ key, path, labelKey, Icon }) => {
          const on = active === key;
          return (
            <Link
              key={key}
              to={path}
              aria-current={on ? "page" : undefined}
              className={`relative flex flex-col items-center gap-1.5 rounded-[14px] px-1 py-[11px] transition-colors duration-150 ${
                on
                  ? "bg-white/[0.07] text-white"
                  : "text-fg-meta hover:bg-white/[0.04] hover:text-slate-300"
              }`}
            >
              {on && (
                <span
                  aria-hidden
                  className="absolute left-0 top-3.5 bottom-3.5 w-[3px] rounded-r-[3px] bg-white/70"
                />
              )}
              <Icon
                className={`h-7 w-7 transition-opacity duration-150 ${on ? "opacity-100" : "opacity-50"}`}
              />
              <span className="text-[10.5px] font-semibold tracking-[0.02em]">
                {t(labelKey)}
              </span>
            </Link>
          );
        })}
      </div>

      <div className="mt-auto flex w-full flex-col items-center gap-2 border-t border-hairline-strong pt-2.5">
        <button
          onClick={onSettings}
          className="flex w-full flex-col items-center gap-1 rounded-[14px] px-1 py-2 text-fg-meta transition-colors hover:bg-white/[0.04] hover:text-slate-300"
        >
          <img src="/icons/sections/settings.png" alt="" aria-hidden="true" draggable={false} className="h-6 w-6 opacity-50" />
          <span className="text-[10.5px] font-semibold tracking-[0.02em]">{t("nav.settings")}</span>
        </button>

        {/* Labelled social badges: brand icon on the left, name on the right. */}
        <div className="flex w-full flex-col gap-1">
          {SOCIAL_LINKS.map(({ key, url, label, Glyph, hover }) => (
            <a
              key={key}
              href={url}
              onClick={social(url)}
              aria-label={label}
              title={label}
              className={`flex w-full items-center gap-1.5 rounded-[10px] border border-hairline px-1.5 py-1.5 text-fg-meta transition-colors ${hover}`}
            >
              <Glyph />
              <span className="text-[10px] font-semibold tracking-[0.01em]">{label}</span>
            </a>
          ))}
        </div>

        <BuildBadge compact />
      </div>
    </nav>
  );
}
