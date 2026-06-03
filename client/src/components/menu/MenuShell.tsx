import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { useInShell } from "../chrome/ShellContext";

interface MenuShellProps {
  eyebrow?: string;
  title?: string;
  description?: string;
  /** Custom header node, rendered in place of the eyebrow/title/description for
   *  panes whose masthead doesn't fit that model (e.g. the home dashboard's
   *  brand lockup). Lets every pane share this one centered layout. */
  header?: ReactNode;
  aside?: ReactNode;
  children: ReactNode;
  layout?: "split" | "stacked";
  /** Tailwind max-width class for the embedded content column. Defaults to the
   *  home dashboard's `max-w-[1180px]` so every pane centers identically as the
   *  viewport widens. Override per-pane (e.g. a narrow lobby form). */
  contentWidthClass?: string;
}

export function MenuShell({
  eyebrow,
  title,
  description,
  header,
  aside,
  children,
  layout = "split",
  contentWidthClass,
}: MenuShellProps) {
  // Inside the modern shell every pane reads left-aligned and top-anchored, the
  // way the design-system handoff embeds them (Scene's `embedded ? flex-start :
  // center`). A page's stacked/split choice only governs its *standalone* look;
  // the shell forces the left-aligned, full-width presentation regardless.
  const embedded = useInShell();
  const centered = layout === "stacked" && !embedded;
  // Match the home dashboard exactly: a single centered container the content
  // fills, so header and content share a left edge AND the whole block stays
  // centered as the viewport grows (vs. a narrow block pinned left in a wider
  // container, which drifts off-centre).
  const widthClass = embedded ? (contentWidthClass ?? "max-w-[1180px]") : "max-w-7xl";
  // Headerless panes (e.g. the multi-phase draft flows that render their own
  // per-phase titles) pass only children. Skip the masthead `<section>` entirely
  // so they don't inherit a stray `gap-8` above their content.
  const hasHeader = Boolean(header || eyebrow || title || description || aside);

  return (
    <div
      className={[
        "relative z-10 mx-auto flex w-full flex-col justify-start",
        widthClass,
        embedded ? "px-6 py-9 lg:px-9" : "min-h-screen px-6 py-16 lg:px-10",
      ].join(" ")}
    >
      <div
        className={centered
          ? "flex flex-col items-center gap-8"
          : layout === "stacked"
            ? "flex flex-col items-start gap-8"
            : "grid items-start gap-8 lg:grid-cols-[minmax(0,0.84fr)_minmax(0,1.16fr)]"}
      >
        {hasHeader && (
          <section className={`flex w-full flex-col ${centered ? "items-center" : "items-start"}`}>
            {header ?? (
              <>
                {eyebrow && (
                  <div className="menu-kicker text-amber-100/58">
                    {eyebrow}
                  </div>
                )}
                {title && (
                  <h1
                    className={[
                      "menu-display text-balance text-[2.4rem] leading-[1.02] text-white sm:text-[3.1rem]",
                      eyebrow ? "mt-4" : "",
                      centered ? "max-w-3xl text-center" : "max-w-xl",
                    ].join(" ")}
                  >
                    {title}
                  </h1>
                )}
                {description && (
                  <p
                    className={[
                      "mt-4 text-[0.97rem] leading-7 text-slate-400",
                      centered ? "max-w-3xl text-center" : "max-w-2xl",
                    ].join(" ")}
                  >
                    {description}
                  </p>
                )}
              </>
            )}
            {aside && (
              <div className={`mt-6 w-full ${centered ? "max-w-4xl" : ""}`}>
                {aside}
              </div>
            )}
          </section>
        )}

        <section className={centered ? "flex w-full max-w-5xl justify-center" : "w-full"}>{children}</section>
      </div>
      {!embedded && <MenuFooterDisclaimer />}
    </div>
  );
}

function MenuFooterDisclaimer() {
  const { t } = useTranslation("menu");
  return (
    <p className="mt-16 text-center text-[0.68rem] leading-5 text-slate-500/70">
      {t("shell.disclaimer")}
    </p>
  );
}

interface MenuPanelProps {
  children: ReactNode;
  className?: string;
}

export function MenuPanel({ children, className }: MenuPanelProps) {
  return (
    <div
      className={[
        "rounded-[22px] border border-white/10 bg-black/18 p-5 shadow-[0_18px_54px_rgba(0,0,0,0.22)] backdrop-blur-md",
        className,
      ].filter(Boolean).join(" ")}
    >
      {children}
    </div>
  );
}

interface MenuShortcutHintProps {
  label: string;
  value: string;
}

export function MenuShortcutHint({ label, value }: MenuShortcutHintProps) {
  return (
    <div className="flex items-center justify-between gap-3 rounded-2xl border border-white/8 bg-black/16 px-4 py-3">
      <span className="text-[0.68rem] uppercase tracking-[0.24em] text-slate-500">{label}</span>
      <span className="rounded-full border border-white/10 bg-white/6 px-3 py-1 text-xs font-medium tracking-[0.18em] text-slate-200">
        {value}
      </span>
    </div>
  );
}
