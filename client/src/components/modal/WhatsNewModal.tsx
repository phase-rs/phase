import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { ChangelogEntry, ChangelogTag } from "../../services/changelog";
import { DialogShell } from "./DialogShell";

interface WhatsNewModalProps {
  entries: ChangelogEntry[];
  loading: boolean;
  failed: boolean;
  onRetry: () => void;
  onClose: () => void;
}

/** Entries shown per page. The feed is append-only and unbounded, so the modal
 * windows it rather than rendering the whole history in one scroll. */
const PAGE_SIZE = 10;

/**
 * One entry's body, clamped to 3 lines (the `line-clamp-3` class) with a Show
 * more / Show less toggle. The toggle is offered only when the text actually
 * overflows the clamp — measured against the live layout — so single-line
 * entries never get a dead control. Body is engine/skill-authored plain text
 * rendered as React text children (auto-escaped); `whitespace-pre-line`
 * preserves the emoji-section newlines. Never dangerouslySetInnerHTML.
 */
function ChangelogBody({ body }: { body: string }) {
  const { t } = useTranslation("menu");
  const ref = useRef<HTMLParagraphElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [overflows, setOverflows] = useState(false);

  // Measure while collapsed: a clamped element reports the full content height
  // in scrollHeight but only the clamped height in clientHeight. jsdom reports
  // both as 0, so tests simply see the (always-present) full text and no toggle.
  useLayoutEffect(() => {
    const el = ref.current;
    if (el) setOverflows(el.scrollHeight > el.clientHeight + 1);
  }, [body]);

  return (
    <div className="mt-3">
      <p
        ref={ref}
        className={`whitespace-pre-line text-sm leading-relaxed text-slate-300 ${
          expanded ? "" : "line-clamp-3"
        }`}
      >
        {body}
      </p>
      {overflows && (
        <button
          type="button"
          onClick={() => setExpanded((e) => !e)}
          className="mt-1 text-xs font-medium text-slate-400 transition hover:text-slate-200"
        >
          {expanded ? t("whatsNew.showLess") : t("whatsNew.showMore")}
        </button>
      )}
    </div>
  );
}

/**
 * Per-tag presentation: i18n label key + a Tailwind chip palette. A closed
 * Record over the ChangelogTag union means adding a tag without giving it a
 * style is a compile error — the lookup can never miss at runtime.
 */
const TAG_META: Record<ChangelogTag, { labelKey: string; chipClass: string }> = {
  "new-cards": { labelKey: "whatsNew.tag.new-cards", chipClass: "bg-emerald-500/15 text-emerald-300" },
  "card-fixes": { labelKey: "whatsNew.tag.card-fixes", chipClass: "bg-amber-500/15 text-amber-300" },
  gameplay: { labelKey: "whatsNew.tag.gameplay", chipClass: "bg-rose-500/15 text-rose-300" },
  interface: { labelKey: "whatsNew.tag.interface", chipClass: "bg-sky-500/15 text-sky-300" },
  localization: { labelKey: "whatsNew.tag.localization", chipClass: "bg-violet-500/15 text-violet-300" },
  ai: { labelKey: "whatsNew.tag.ai", chipClass: "bg-cyan-500/15 text-cyan-300" },
  multiplayer: { labelKey: "whatsNew.tag.multiplayer", chipClass: "bg-fuchsia-500/15 text-fuchsia-300" },
};

export function WhatsNewModal({
  entries,
  loading,
  failed,
  onRetry,
  onClose,
}: WhatsNewModalProps) {
  const { t, i18n } = useTranslation("menu");
  const [query, setQuery] = useState("");
  const [page, setPage] = useState(1);

  const formatDate = (iso: string): string => {
    // Pin to local midnight so the date never slips a day across timezones.
    const d = new Date(`${iso}T00:00:00`);
    if (isNaN(d.getTime())) return iso;
    return d.toLocaleDateString(i18n.language, {
      year: "numeric",
      month: "long",
      day: "numeric",
    });
  };

  // Case-insensitive substring match over title, body, and the *translated* tag
  // labels — so searching "cards" in any locale matches the New Cards section,
  // not just literal body text. Recomputed only when entries, query, or the
  // active language change.
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return entries;
    return entries.filter((entry) => {
      const tagLabels = entry.tags.map((tag) => t(TAG_META[tag].labelKey)).join(" ");
      const haystack = `${entry.title}\n${entry.body}\n${tagLabels}`.toLowerCase();
      return haystack.includes(q);
    });
  }, [entries, query, t]);

  const pageCount = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  // Clamp defensively: a shrinking result set (typing more of a query) must
  // never strand the view on a now-empty page.
  const current = Math.min(page, pageCount);
  const visible = filtered.slice((current - 1) * PAGE_SIZE, current * PAGE_SIZE);

  // A new query restarts paging from the top — the old page index is
  // meaningless against a different result set.
  useEffect(() => {
    setPage(1);
  }, [query]);

  const hasEntries = entries.length > 0;

  return (
    <DialogShell
      eyebrow="phase.rs"
      title={t("whatsNew.title")}
      size="lg"
      scrollable
      onClose={onClose}
    >
      {hasEntries && (
        // Sticky toolbar: search + paging stay pinned while the entry list
        // scrolls beneath them, so the user can re-search or page without
        // scrolling back up. Matches the dialog's own surface colour so the
        // pinned bar reads as chrome, not content.
        <div className="sticky top-0 z-10 border-b border-white/5 bg-[#0b1020]/96 px-3 py-3 backdrop-blur lg:px-5">
          <div className="relative">
            <input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t("whatsNew.searchPlaceholder")}
              aria-label={t("whatsNew.searchPlaceholder")}
              className="w-full rounded-lg border border-white/10 bg-white/[0.04] py-2 pl-9 pr-3 text-sm text-slate-200 placeholder:text-slate-500 focus:border-white/25 focus:outline-none"
            />
            <svg
              viewBox="0 0 20 20"
              fill="currentColor"
              aria-hidden="true"
              className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-slate-500"
            >
              <path
                fillRule="evenodd"
                d="M9 3.5a5.5 5.5 0 1 0 3.4 9.82l3.64 3.64a.75.75 0 1 0 1.06-1.06l-3.64-3.64A5.5 5.5 0 0 0 9 3.5ZM5 9a4 4 0 1 1 8 0 4 4 0 0 1-8 0Z"
                clipRule="evenodd"
              />
            </svg>
          </div>

          {pageCount > 1 && (
            <div className="mt-3 flex items-center justify-between">
              <button
                type="button"
                onClick={() => setPage((p) => Math.max(1, p - 1))}
                disabled={current <= 1}
                className="rounded-lg border border-white/15 px-3 py-1.5 text-xs font-medium text-slate-200 transition hover:bg-white/10 disabled:cursor-not-allowed disabled:opacity-40"
              >
                {t("whatsNew.prevPage")}
              </button>
              <span className="text-xs text-slate-500">
                {t("whatsNew.pageStatus", { current, total: pageCount })}
              </span>
              <button
                type="button"
                onClick={() => setPage((p) => Math.min(pageCount, p + 1))}
                disabled={current >= pageCount}
                className="rounded-lg border border-white/15 px-3 py-1.5 text-xs font-medium text-slate-200 transition hover:bg-white/10 disabled:cursor-not-allowed disabled:opacity-40"
              >
                {t("whatsNew.nextPage")}
              </button>
            </div>
          )}
        </div>
      )}

      <div className="px-3 py-3 lg:px-5 lg:py-4">
        {loading && entries.length === 0 ? (
          <div className="flex items-center justify-center py-12">
            <div className="h-7 w-7 animate-spin rounded-full border-2 border-slate-600 border-t-white" />
            <span className="sr-only">{t("whatsNew.loading")}</span>
          </div>
        ) : failed && entries.length === 0 ? (
          <div className="flex flex-col items-center gap-3 py-10 text-center">
            <p className="text-sm text-slate-400">{t("whatsNew.failed")}</p>
            <button
              type="button"
              onClick={onRetry}
              className="rounded-lg border border-white/15 px-3 py-1.5 text-sm text-slate-200 transition hover:bg-white/10"
            >
              {t("whatsNew.retry")}
            </button>
          </div>
        ) : entries.length === 0 ? (
          <p className="py-10 text-center text-sm text-slate-400">
            {t("whatsNew.empty")}
          </p>
        ) : visible.length === 0 ? (
          <p className="py-10 text-center text-sm text-slate-400">
            {t("whatsNew.noResults")}
          </p>
        ) : (
          <ul className="flex flex-col gap-6">
            {visible.map((entry) => (
              <li key={entry.id} className="border-b border-white/5 pb-6 last:border-0 last:pb-0">
                <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
                  <h3 className="text-base font-semibold text-white lg:text-lg">
                    {entry.title}
                  </h3>
                  <time className="text-xs text-slate-500" dateTime={entry.date}>
                    {formatDate(entry.date)}
                  </time>
                </div>

                {entry.tags.length > 0 && (
                  <div className="mt-2 flex flex-wrap gap-1.5">
                    {entry.tags.map((tag) => (
                      <span
                        key={tag}
                        className={`rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide ${TAG_META[tag].chipClass}`}
                      >
                        {t(TAG_META[tag].labelKey)}
                      </span>
                    ))}
                  </div>
                )}

                <ChangelogBody body={entry.body} />

                {entry.discordUrl && (
                  <a
                    href={entry.discordUrl}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="mt-3 inline-block text-xs font-medium text-indigo-300 hover:text-indigo-200 hover:underline"
                  >
                    {t("whatsNew.discordLink")}
                  </a>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
    </DialogShell>
  );
}
