import type { ReactNode } from "react";

interface MenuShellProps {
  eyebrow?: string;
  title: string;
  description?: string;
  aside?: ReactNode;
  children: ReactNode;
  layout?: "split" | "stacked";
}

export function MenuShell({
  eyebrow,
  title,
  description,
  aside,
  children,
  layout = "split",
}: MenuShellProps) {
  const isStacked = layout === "stacked";

  return (
    <div className="relative z-10 mx-auto flex min-h-screen w-full max-w-7xl flex-col justify-center px-6 py-16 lg:px-10">
      <div
        className={isStacked
          ? "flex flex-col items-center gap-8"
          : "grid items-start gap-8 lg:grid-cols-[minmax(0,0.84fr)_minmax(0,1.16fr)]"}
      >
        <section className={`flex flex-col ${isStacked ? "items-center" : "items-start"}`}>
          {eyebrow && (
            <div className="menu-kicker text-amber-100/58">
              {eyebrow}
            </div>
          )}
          <h1
            className={[
              "menu-display text-balance text-[2.4rem] leading-[1.02] text-white sm:text-[3.1rem]",
              eyebrow ? "mt-4" : "",
              isStacked ? "max-w-3xl text-center" : "max-w-xl",
            ].join(" ")}
          >
            {title}
          </h1>
          {description && (
            <p
              className={[
                "mt-4 text-[0.97rem] leading-7 text-slate-400",
                isStacked ? "max-w-3xl text-center" : "max-w-2xl",
              ].join(" ")}
            >
              {description}
            </p>
          )}
          {aside && (
            <div className={`mt-6 w-full ${isStacked ? "max-w-4xl" : ""}`}>
              {aside}
            </div>
          )}
        </section>

        <section className={isStacked ? "w-full max-w-5xl" : "w-full"}>{children}</section>
      </div>
    </div>
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
