import { useTranslation } from "react-i18next";

import { useStatusMessage } from "../../hooks/useStatusMessage";
import { openExternal } from "../../services/openExternal";
import type { StatusSeverity } from "../../services/status";
import { usePreferencesStore } from "../../stores/preferencesStore";

/**
 * Severity → visual tone, ARIA role, and eyebrow key. `critical` is the only
 * severity that interrupts a screen reader (`alert`); info/warning announce
 * politely, matching `DeckLegalityChip`'s split between a legality result and a
 * legality failure. The class strings are literals so Tailwind's source scan
 * sees them.
 */
const TONE = {
  info: {
    box: "border-sky-400/20 bg-sky-500/[0.07]",
    text: "text-sky-200",
    subtle: "text-sky-200/80",
    role: "status",
    dismiss: "text-sky-200/70 hover:text-sky-200",
    eyebrow: "chrome.statusBanner.severity.info",
  },
  warning: {
    box: "border-amber-400/25 bg-amber-500/[0.08]",
    text: "text-amber-200",
    subtle: "text-amber-200/80",
    role: "status",
    dismiss: "text-amber-200/70 hover:text-amber-200",
    eyebrow: "chrome.statusBanner.severity.warning",
  },
  critical: {
    box: "border-rose-400/20 bg-rose-500/[0.07]",
    text: "text-rose-200",
    subtle: "text-rose-200/80",
    role: "alert",
    dismiss: "text-rose-200/70 hover:text-rose-200",
    eyebrow: "chrome.statusBanner.severity.critical",
  },
} as const satisfies Record<
  StatusSeverity,
  {
    box: string;
    text: string;
    subtle: string;
    dismiss: string;
    role: "status" | "alert";
    eyebrow: string;
  }
>;

/**
 * The operator status banner — an out-of-band message from the maintainer
 * (outage, maintenance window, known issue) shown above the page content.
 *
 * Propless and self-sourcing (the `PodErrorBanner` convention): it owns the
 * fetch/poll/expiry/dismissal logic via `useStatusMessage` and renders nothing
 * when there is nothing live to show. WHERE it appears is the shell's decision,
 * not this component's — `AppShell` gates the mount to the two surfaces the
 * message targets.
 *
 * The author's text is shown verbatim in every locale (only the chrome — the
 * eyebrow and the dismiss label — is translated), and `body` renders as TEXT,
 * never HTML.
 */
export function StatusBanner() {
  const { t } = useTranslation();
  const message = useStatusMessage();
  const setDismissedStatusId = usePreferencesStore((s) => s.setDismissedStatusId);

  if (!message) return null;

  const tone = TONE[message.severity];
  const link = message.link;

  return (
    <div className="px-2 pt-2 min-[820px]:px-4 min-[820px]:pt-3">
      <div
        role={tone.role}
        className={`mx-auto flex w-full max-w-3xl items-start gap-3 rounded-[10px] border px-4 py-2.5 shadow-[0_8px_22px_rgba(0,0,0,0.18)] backdrop-blur-sm ${tone.box}`}
      >
        <div className="flex min-w-0 flex-1 flex-col">
          <span className={`text-[10px] font-semibold uppercase tracking-wide ${tone.subtle}`}>
            {t(tone.eyebrow)}
          </span>
          <span className={`text-xs font-medium ${tone.text}`}>{message.title}</span>
          <p className={`mt-1 text-[11px] leading-5 ${tone.subtle}`}>{message.body}</p>
          {link && (
            // A button, not an <a>: the desktop webview opens nothing from a
            // bare target="_blank", and openExternal is the single URL authority
            // that rejects a non-http(s) href from a published payload.
            <button
              type="button"
              onClick={() => openExternal(link.url)}
              className={`mt-1 self-start text-[11px] font-medium underline underline-offset-2 ${tone.text}`}
            >
              {link.label}
            </button>
          )}
        </div>
        {message.dismissible && (
          <button
            type="button"
            onClick={() => setDismissedStatusId(message.id)}
            aria-label={t("chrome.statusBanner.dismiss")}
            className={`min-h-11 min-w-11 shrink-0 text-lg leading-none transition-colors ${tone.dismiss}`}
          >
            &times;
          </button>
        )}
      </div>
    </div>
  );
}
