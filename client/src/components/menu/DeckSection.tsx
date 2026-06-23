import type { ReactNode } from "react";

interface DeckSectionProps {
  title: string;
  count: number;
  /** Leading glyph (e.g. star for the Starred section, lock for system folders). */
  icon?: ReactNode;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** Right-aligned header control (folder kebab, "Manage feeds", "Browse all"). */
  headerAction?: ReactNode;
  /** Shown in place of the body when the section is expanded but has no decks. */
  emptyHint?: string;
  children: ReactNode;
}

/**
 * One collapsible deck-library section. Shared chrome for user folders, the
 * virtual Starred/Unfiled buckets, and the immutable system folders (Starter,
 * Precons) — so the whole library speaks one visual language and every section
 * participates in Collapse-All / Expand-All.
 */
export function DeckSection({
  title,
  count,
  icon,
  collapsed,
  onToggleCollapsed,
  headerAction,
  emptyHint,
  children,
}: DeckSectionProps) {
  return (
    <section>
      <div className="mb-3 flex items-center justify-between gap-3">
        <button
          type="button"
          onClick={onToggleCollapsed}
          aria-expanded={!collapsed}
          className="group/section flex min-w-0 items-center gap-2 text-xs font-semibold uppercase tracking-wider text-slate-400 transition-colors hover:text-slate-200"
        >
          <svg
            viewBox="0 0 16 16"
            fill="currentColor"
            aria-hidden="true"
            className={`h-3.5 w-3.5 shrink-0 text-slate-500 transition-transform duration-150 ${
              collapsed ? "-rotate-90" : ""
            }`}
          >
            <path d="M4.22 6.22a.75.75 0 0 1 1.06 0L8 8.94l2.72-2.72a.75.75 0 1 1 1.06 1.06l-3.25 3.25a.75.75 0 0 1-1.06 0L4.22 7.28a.75.75 0 0 1 0-1.06Z" />
          </svg>
          {icon}
          <span className="min-w-0 truncate">{title}</span>
          <span className="text-slate-600">{count}</span>
        </button>
        {headerAction}
      </div>
      {!collapsed &&
        (count === 0 && emptyHint ? (
          <p className="rounded-lg border border-dashed border-white/10 px-3 py-4 text-center text-xs text-slate-500">
            {emptyHint}
          </p>
        ) : (
          children
        ))}
    </section>
  );
}
