import { useTranslation } from "react-i18next";

import { isSupabaseConfigured } from "../../services/cloudSync/supabaseClient";
import { useCloudSyncStore } from "../../stores/cloudSyncStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { ModalPanelShell } from "../ui/ModalPanelShell";
import { DiscordIcon, GoogleIcon } from "../ui/ProviderIcons";

function UsersGlyph() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-8 w-8 fill-current">
      <path d="M16 11c1.66 0 3-1.34 3-3s-1.34-3-3-3-3 1.34-3 3 1.34 3 3 3Zm-8 0c1.66 0 3-1.34 3-3S9.66 5 8 5 5 6.34 5 8s1.34 3 3 3Zm0 2c-2.33 0-7 1.17-7 3.5V19h14v-2.5C15 14.17 10.33 13 8 13Zm8 0c-.29 0-.62.02-.97.05A4.5 4.5 0 0 1 17 16.5V19h6v-2.5c0-2.33-4.67-3.5-7-3.5Z" />
    </svg>
  );
}

const ACTION_BTN =
  "w-full rounded-[12px] border border-hairline bg-white/5 px-3 py-2 text-sm font-medium text-slate-100 transition hover:bg-white/10";

/**
 * Minimal, real-data profile: the local display name (multiplayer identity) and,
 * where cloud sync is configured, the signed-in account with sign-out. There is
 * no account system beyond this — deliberately no friend code or stat tiles,
 * since the engine tracks none. Cloud-sync state is the single source shared with
 * the AccountControl chrome indicator and the Settings → Data section.
 */
export function ProfileModal({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation("settings");
  const displayName = useMultiplayerStore((s) => s.displayName);
  const setDisplayName = useMultiplayerStore((s) => s.setDisplayName);

  const identity = useCloudSyncStore((s) => s.identity);
  const sessionResolved = useCloudSyncStore((s) => s.sessionResolved);
  const signIn = useCloudSyncStore((s) => s.signIn);
  const signOut = useCloudSyncStore((s) => s.signOut);
  const syncConfigured = isSupabaseConfigured();

  return (
    <ModalPanelShell
      eyebrow={t("profile.eyebrow")}
      title={t("profile.title")}
      subtitle={t("profile.subtitle")}
      onClose={onClose}
      maxWidthClassName="max-w-lg"
      bodyClassName="overflow-y-auto p-4 sm:p-6"
    >
      <div className="flex items-center gap-4">
        <div className="flex h-16 w-16 shrink-0 items-center justify-center rounded-tile border border-hairline-strong bg-surface-panel-strong text-stone">
          <UsersGlyph />
        </div>
        <label className="min-w-0 flex-1">
          <span className="mb-1.5 block text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-fg-meta">
            {t("multiplayer.displayName")}
          </span>
          <input
            type="text"
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
            placeholder={t("multiplayer.displayNamePlaceholder")}
            maxLength={20}
            className="w-full rounded-[14px] border border-hairline bg-black/18 px-3 py-2 text-base text-slate-100 placeholder-slate-500 focus:border-arcane/40 focus:outline-none"
          />
        </label>
      </div>

      {syncConfigured && (
        <div className="mt-6 rounded-card border border-hairline bg-surface-panel p-4">
          <div className="text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-fg-meta">
            {t("sync.title")}
          </div>
          <p className="mt-2 text-xs text-fg-muted">{t("sync.description")}</p>

          {!sessionResolved ? (
            <p className="mt-3 text-xs text-fg-meta">{t("sync.statusSyncing")}</p>
          ) : identity ? (
            <div className="mt-3 flex flex-col gap-3">
              <div className="flex items-center gap-2">
                {identity.avatarUrl && (
                  <img src={identity.avatarUrl} alt="" className="h-7 w-7 rounded-full" referrerPolicy="no-referrer" />
                )}
                <span className="text-sm text-slate-200">
                  {t("sync.signedInAs", { name: identity.label })}
                </span>
              </div>
              <button className={`${ACTION_BTN} text-fg-muted`} onClick={() => void signOut()}>
                {t("sync.signOut")}
              </button>
            </div>
          ) : (
            <div className="mt-3 flex flex-col gap-2">
              <button className={ACTION_BTN} onClick={() => void signIn("discord")}>
                <span className="flex items-center justify-center gap-2">
                  <DiscordIcon className="h-4 w-4" />
                  {t("sync.signInWith", { provider: t("sync.providerDiscord") })}
                </span>
              </button>
              <button className={ACTION_BTN} onClick={() => void signIn("google")}>
                <span className="flex items-center justify-center gap-2">
                  <GoogleIcon className="h-4 w-4" />
                  {t("sync.signInWith", { provider: t("sync.providerGoogle") })}
                </span>
              </button>
            </div>
          )}
        </div>
      )}
    </ModalPanelShell>
  );
}
