import { useEffect, useRef, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { menuButtonClass } from "../menu/buttonStyles";
import { DiscordIcon, GoogleIcon } from "../ui/ProviderIcons";
import { useCloudSyncStore } from "../../stores/cloudSyncStore";
import { isSupabaseConfigured } from "../../services/cloudSync/supabaseClient";

// Heroicons-solid cloud path — visually centered in the 24-unit viewBox (Y
// range ~3.5–20.25, midpoint ≈ 11.9), unlike a top-biased "minimal" path.
function CloudIcon({ className }: { className?: string }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 24 24"
      fill="currentColor"
      className={className ?? "h-6 w-6"}
      aria-hidden="true"
    >
      <path
        fillRule="evenodd"
        d="M4.5 9.75a6 6 0 0 1 11.573-2.226 3.75 3.75 0 0 1 4.133 4.303A4.5 4.5 0 0 1 18 20.25H6.75a5.25 5.25 0 0 1-2.23-10.004 6.072 6.072 0 0 1-.02-.496Z"
        clipRule="evenodd"
      />
    </svg>
  );
}

// Heroicons "arrow-path" — two curved arrows forming a recycle/sync glyph.
// Used as the spinning overlay badge while a sync is in flight.
function RefreshIcon({ className }: { className?: string }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className ?? "h-4 w-4"}
      aria-hidden="true"
    >
      <path d="M16.023 9.348h4.992V4.356M2.985 19.644v-4.992h4.992m-4.992 0 3.181 3.183a8.25 8.25 0 0 0 13.803-3.7M4.031 9.865a8.25 8.25 0 0 1 13.803-3.7l3.181 3.182" />
    </svg>
  );
}

// Outlined cloud + diagonal slash — visually distinct from the filled cloud,
// reads as "not connected / off" at a glance.
function CloudOffIcon({ className }: { className?: string }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className ?? "h-6 w-6"}
      aria-hidden="true"
    >
      <path d="M2.25 15a4.5 4.5 0 0 0 4.5 4.5H18a3.75 3.75 0 0 0 1.332-7.257 3 3 0 0 0-3.758-3.848 5.25 5.25 0 0 0-10.233 2.33A4.502 4.502 0 0 0 2.25 15Z" />
      <path d="M3 3l18 18" />
    </svg>
  );
}

function Spinner({ className }: { className?: string }) {
  return (
    <svg
      className={className ?? "h-3.5 w-3.5 animate-spin"}
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <circle
        className="opacity-25"
        cx="12"
        cy="12"
        r="10"
        stroke="currentColor"
        strokeWidth="4"
      />
      <path
        className="opacity-75"
        fill="currentColor"
        d="M4 12a8 8 0 0 1 8-8v4a4 4 0 0 0-4 4H4z"
      />
    </svg>
  );
}

const POPOVER_BTN =
  "w-full rounded-[12px] border border-white/10 bg-white/5 px-3 py-2 text-sm font-medium text-slate-100 transition hover:bg-white/10 disabled:cursor-not-allowed disabled:opacity-50";

/**
 * App-wide account affordance in the ScreenChrome cluster: a sign-in entry point
 * (signed out) or an avatar with live sync status (signed in), opening a popover
 * with the full cloud-sync controls. Renders nothing on deployments with no
 * provider configured, so self-hosters see no account UI. All logic lives in
 * useCloudSyncStore — this is one of two renderings of it (the other is the
 * Settings → Data section).
 */
export function AccountControl() {
  const { t } = useTranslation("settings");
  const identity = useCloudSyncStore((s) => s.identity);
  const sessionResolved = useCloudSyncStore((s) => s.sessionResolved);
  const status = useCloudSyncStore((s) => s.status);
  const error = useCloudSyncStore((s) => s.error);
  const lastSyncedAt = useCloudSyncStore((s) => s.lastSyncedAt);
  const conflict = useCloudSyncStore((s) => s.conflict);
  const conflictDiff = useCloudSyncStore((s) => s.conflictDiff);
  const dirty = useCloudSyncStore((s) => s.dirty);
  const signIn = useCloudSyncStore((s) => s.signIn);
  const signOut = useCloudSyncStore((s) => s.signOut);
  const syncNow = useCloudSyncStore((s) => s.syncNow);
  const resolveConflict = useCloudSyncStore((s) => s.resolveConflict);
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [open]);

  // Auto-open the popover whenever a new conflict appears so the user is
  // surfaced the keep-cloud/keep-local decision immediately — critical on
  // first sign-in on a new device, where the yellow icon alone gives no hint
  // that action is required. Re-runs only when `conflict` itself changes, so
  // closing the popover doesn't fight the user (effect won't re-fire until
  // the conflict resolves and a new one arrives).
  useEffect(() => {
    if (conflict) setOpen(true);
  }, [conflict]);

  // Hard gate on the build-time config, not on a runtime store flag. Once the
  // bundle has Supabase URL/key baked in, this check is constant `true` for
  // the lifetime of the page — so the button cannot vanish mid-session due to
  // any store-state transition. Self-hosters without Supabase configured
  // statically evaluate to `false` and the button is never mounted at all.
  if (!isSupabaseConfigured()) return null;

  const syncing = status === "syncing";

  // State lives in the cloud-icon color only — the button shell matches its
  // chrome neighbors so it doesn't shout. Semantics, brightest to dimmest:
  //   not connected → outlined cloud + slash, slate (off)
  //   error         → rose
  //   conflict      → amber (bright)
  //   syncing       → cyan + animate-pulse
  //   dirty         → amber (local diverges from cloud, pending push)
  //   synced        → emerald + soft glow (signed in, local matches cloud).
  //                    The drop-shadow gives the calm/confirmed state real
  //                    presence on the chrome row so it doesn't blend into
  //                    the neutral volume/flag/settings neighbors.
  // Tri-state for "show signed-out UI": ONLY when restoreSession has settled
  // AND there's no identity. Until then we keep the icon neutral so a signed-
  // in user doesn't see a "Sign in" flash between mount and async session
  // restore. `identity` alone is insufficient — it's null in both "unknown"
  // and "confirmed signed-out" cases.
  const signedOut = sessionResolved && !identity;
  const synced = Boolean(identity) && !syncing && !dirty && !conflict && status !== "error";
  const iconColor = !identity
    ? "text-slate-400"
    : status === "error"
      ? "text-rose-400"
      : conflict
        ? "text-amber-500"
        : syncing
          ? "text-cyan-300"
          : dirty
            ? "text-amber-400"
            : "text-emerald-300";
  const iconGlow = synced
    ? "drop-shadow-[0_0_5px_rgba(52,211,153,0.55)]"
    : "";

  const statusLine =
    status === "error" ? (
      <span className="text-rose-400">
        {t("sync.statusError")}
        {error ? `: ${error}` : ""}
      </span>
    ) : syncing ? (
      t("sync.statusSyncing")
    ) : (
      t("sync.lastSynced", {
        time: lastSyncedAt
          ? new Date(lastSyncedAt).toLocaleString()
          : t("sync.never"),
      })
    );

  return (
    <div ref={ref} className="relative">
      <motion.button
        className={menuButtonClass({
          tone: "neutral",
          size: "chrome",
          className: "relative",
        })}
        whileHover={{ y: -1 }}
        whileTap={{ scale: 0.98 }}
        onClick={() => setOpen((v) => !v)}
        aria-label={t("sync.title")}
        title={t("sync.title")}
      >
        {/* Render the off-slash icon only when we've *confirmed* signed-out.
            During the brief async window between mount and restoreSession
            settling, render the neutral CloudIcon so a signed-in user never
            sees a "Sign in" flash. */}
        {signedOut ? (
          <CloudOffIcon className={`h-5 w-5 ${iconColor}`} />
        ) : (
          <CloudIcon className={`h-5 w-5 ${iconColor} ${iconGlow}`} />
        )}
        {syncing && (
          <span
            className="absolute top-0.5 right-0.5 flex h-4 w-4 items-center justify-center rounded-full border border-cyan-400/40 bg-slate-900"
            aria-hidden="true"
          >
            <RefreshIcon className="h-3 w-3 animate-spin text-cyan-300" />
          </span>
        )}
      </motion.button>

      {open && (
        <div className="absolute right-0 top-full z-40 mt-2 w-72 max-w-[calc(100vw-2rem)] rounded-[16px] border border-white/12 bg-slate-900/95 p-4 shadow-[0_18px_54px_rgba(0,0,0,0.4)] backdrop-blur-md">
          <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-slate-500">
            {t("sync.title")}
          </h3>
          <p className="mt-2 text-xs text-slate-400">{t("sync.description")}</p>
          <p className="mt-1 text-xs text-slate-500">{t("sync.savesNote")}</p>

          {!sessionResolved ? (
            // Session restore in flight — keep the popover content empty
            // rather than guess at a signed-in/out shape we don't know yet.
            // Same async window the icon swap is guarding against.
            <p className="mt-3 text-xs text-slate-500">{t("sync.statusSyncing")}</p>
          ) : !identity ? (
            <div className="mt-3 flex flex-col gap-2">
              <button
                className={POPOVER_BTN}
                onClick={() => void signIn("discord")}
              >
                <span className="flex items-center justify-center gap-2">
                  <DiscordIcon className="h-4 w-4" />
                  {t("sync.signInWith", {
                    provider: t("sync.providerDiscord"),
                  })}
                </span>
              </button>
              <button
                className={POPOVER_BTN}
                onClick={() => void signIn("google")}
              >
                <span className="flex items-center justify-center gap-2">
                  <GoogleIcon className="h-4 w-4" />
                  {t("sync.signInWith", {
                    provider: t("sync.providerGoogle"),
                  })}
                </span>
              </button>
            </div>
          ) : (
            <div className="mt-3 flex flex-col gap-3">
              <div className="flex items-center gap-2">
                {identity.avatarUrl && (
                  <img
                    src={identity.avatarUrl}
                    alt=""
                    className="h-6 w-6 rounded-full"
                    referrerPolicy="no-referrer"
                  />
                )}
                <span className="text-sm text-slate-200">
                  {t("sync.signedInAs", { name: identity.label })}
                </span>
              </div>

              {conflict ? (
                <div className="flex flex-col gap-2 rounded-[12px] border border-amber-400/30 bg-amber-400/10 p-2.5">
                  <p className="text-xs font-medium text-amber-200">
                    {t("sync.conflictTitle")}
                  </p>
                  <p className="text-xs text-amber-100/80">
                    {t("sync.conflictBody")}
                  </p>
                  {conflictDiff && (
                    <ul className="space-y-0.5 text-[0.7rem] text-amber-100/70">
                      {(conflictDiff.decksAdded > 0 ||
                        conflictDiff.decksModified > 0 ||
                        conflictDiff.decksRemoved > 0) && (
                        <li>
                          {t("sync.diffDecks", {
                            added: conflictDiff.decksAdded,
                            modified: conflictDiff.decksModified,
                            removed: conflictDiff.decksRemoved,
                          })}
                        </li>
                      )}
                      {conflictDiff.prefsChanged && (
                        <li>{t("sync.diffPrefs")}</li>
                      )}
                      {conflictDiff.feedsChanged && (
                        <li>{t("sync.diffFeeds")}</li>
                      )}
                      {conflictDiff.otherChanged && (
                        <li>{t("sync.diffOther")}</li>
                      )}
                    </ul>
                  )}
                  <div className="flex flex-col gap-2">
                    <button
                      className={POPOVER_BTN}
                      onClick={() => void resolveConflict("cloud")}
                    >
                      {t("sync.keepCloud")}
                    </button>
                    <button
                      className={POPOVER_BTN}
                      onClick={() => void resolveConflict("local")}
                    >
                      {t("sync.keepLocal")}
                    </button>
                  </div>
                </div>
              ) : (
                <button
                  className={POPOVER_BTN}
                  disabled={syncing}
                  onClick={() => void syncNow()}
                >
                  <span className="flex items-center justify-center gap-2">
                    {syncing && <Spinner />}
                    {t("sync.syncNow")}
                  </span>
                </button>
              )}

              <p className="text-xs text-slate-500">{statusLine}</p>

              <button
                className={`${POPOVER_BTN} text-slate-400`}
                onClick={() => void signOut()}
              >
                {t("sync.signOut")}
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
