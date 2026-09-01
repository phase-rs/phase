import { Suspense, useRef, useState } from "react";
import { Link, Outlet, useLocation } from "react-router";
import { useTranslation } from "react-i18next";

import { useChangelog } from "../../hooks/useChangelog";
import { SceneParticles } from "../menu/MenuParticles";
import { DraftSteps } from "../draft/DraftSteps";
import { WhatsNewModal } from "../modal/WhatsNewModal";
import { CardDataLoadingBar } from "./CardDataLoadingBar";
import { ChromeControls } from "./ChromeControls";
import { Rail } from "./Rail";
import { DraftShellChromeProvider, ShellProvider, type DraftShellChromeConfig } from "./ShellContext";
import { SocialBar } from "./SocialBar";
import { StatusBanner } from "./StatusBanner";
import { TabBar } from "./TabBar";
import { HomeIcon } from "./navIcons";

/**
 * The modern app shell — a React Router layout route wrapping every out-of-match
 * surface. It renders the atmospheric scene ONCE (backdrop + particles, instead
 * of each page re-mounting its own), the persistent rail (≥820px) / bottom tab
 * bar (<820px), and the shared control cluster, then routes the active page into
 * the offset content column via <Outlet/>. ShellProvider tells embedded pages to
 * drop their own scene/back-button chrome. The full-screen /game/:id route lives
 * outside this shell.
 */
export function AppShell() {
  const { t } = useTranslation("menu");
  // The shell owns settings-modal state so the rail's Settings button and the
  // (controlled) ChromeControls cog share one PreferencesModal instance.
  const [settingsOpen, setSettingsOpen] = useState(false);
  const settingsReturnFocusRef = useRef<HTMLButtonElement>(null);

  // "What's New": the unread dot lives on the rail, the modal is shell-owned.
  const [whatsNewOpen, setWhatsNewOpen] = useState(false);
  const [draftChromeConfig, setDraftChromeConfig] = useState<DraftShellChromeConfig>({ mode: "default" });
  const { mode: draftChromeMode, phoneAction, showProgress = true, topActions = [] } = draftChromeConfig;
  const phoneDraftChrome = draftChromeMode === "phone-drafting" || draftChromeMode === "phone-deckbuilding";
  const draftTopRowChrome = phoneDraftChrome || draftChromeMode === "tablet-drafting";
  const responsiveDraftChrome = draftChromeMode !== "default";
  const shellDraftPhase = draftChromeMode === "phone-deckbuilding" || draftChromeMode === "tablet-deckbuilding"
    ? "deckbuilding"
    : "drafting";
  const changelog = useChangelog();
  // The operator status banner targets exactly the landing surface and the
  // online lobby, so the shell owns the route gate rather than the (propless)
  // banner: keeping it here means the banner's fetch + poll never start on any
  // other shell route. The gate is also load-bearing for layout — the deck
  // builder shell is sized with a hard calc(100dvh - …) that a block-level
  // banner would silently overflow — so widening it is not free.
  const { pathname } = useLocation();
  const showStatusBanner = pathname === "/" || pathname === "/multiplayer";
  const openWhatsNew = () => {
    setWhatsNewOpen(true);
    changelog.openAndLoad();
  };

  return (
    <ShellProvider value={true}>
      <DraftShellChromeProvider value={setDraftChromeConfig}>
        {/* The scene IS the relative root (matching how each page mounts it). NOTE:
          `.menu-scene` is unlayered CSS, which in Tailwind v4 outranks utilities,
          so it must not share an element with a conflicting position utility —
          keep it the relative container and let children position within it. The
          single scene here replaces every page's own (neutralized via
          `.shell-content .menu-scene` in index.css). */}
      {/* `overflow-x-clip` (not `-hidden`): the scene's only off-edge bleed is
          horizontal (moon at left:82-96%, sigils at ±12rem), so x-clip contains
          it — but unlike `overflow-hidden` it does NOT establish a scroll
          container, so the document stays the scroll container and the sticky
          rail/top row below pin correctly. */}
        <div className={`menu-scene relative flex flex-col overflow-x-clip ${responsiveDraftChrome ? "h-dvh min-h-0 overflow-y-hidden" : "min-h-screen"}`}>
        <SceneParticles />
        <div className="menu-scene__vignette" />
        <div className="menu-scene__sigil menu-scene__sigil--left" />
        <div className="menu-scene__sigil menu-scene__sigil--right" />
        <div className="menu-scene__haze" />

        <CardDataLoadingBar />

        {/* Rail (≥820px) + body column. Both the rail and the top chrome row
            occupy real layout space (sticky), so page content can never slide
            under them — no ml/pt reserves, no z-index races for in-flow chrome. */}
        <div className={`relative z-10 flex ${responsiveDraftChrome ? "h-full min-h-0" : "min-h-screen"}`}>
          {!phoneDraftChrome && (
            <Rail
              onSettings={(launcher) => {
                settingsReturnFocusRef.current = launcher;
                setSettingsOpen(true);
              }}
              onWhatsNew={openWhatsNew}
              hasUnread={changelog.hasUnread}
            />
          )}
          <div className="flex min-h-0 min-w-0 flex-1 flex-col">
            {/* Sticky top chrome row: hosts the social strip and reserves the
                vertical band the fixed top-right ChromeControls occupy (44px
                mobile / 56px desktop), so page content clears both. */}
            <div className={phoneDraftChrome
              ? "sticky top-0 z-30 flex min-h-[calc(env(safe-area-inset-top)+52px)] items-center gap-2 px-2 pb-1 pt-[calc(env(safe-area-inset-top)+1rem)]"
              : "sticky top-0 z-30 flex min-h-[calc(env(safe-area-inset-top)+44px)] items-center px-2 pb-1 pt-[calc(env(safe-area-inset-top)+0.5rem)] min-[820px]:min-h-[calc(env(safe-area-inset-top)+56px)] min-[820px]:px-4 min-[820px]:pt-[calc(env(safe-area-inset-top)+0.75rem)]"}
            >
              {draftTopRowChrome && (
                <>
                  <Link
                    to="/"
                    aria-label={t("nav.home")}
                    title={t("nav.home")}
                    className="relative z-10 flex w-11 shrink-0 flex-col items-center justify-center gap-0.5 rounded-[8px] border border-hairline bg-black/45 px-1 py-1 transition-colors hover:border-white/15 hover:bg-slate-950"
                  >
                    <HomeIcon className="h-6 w-6 opacity-70" />
                    <span className="text-[9px] font-semibold leading-none text-fg-meta">{t("nav.home")}</span>
                  </Link>
                  {phoneAction && (
                    <button
                      type="button"
                      onClick={phoneAction.onClick}
                      aria-label={phoneAction.label}
                      title={phoneAction.label}
                      className="relative z-10 flex w-11 shrink-0 items-center justify-center rounded-[8px] border border-hairline bg-black/45 py-2.5 transition-colors hover:border-white/15 hover:bg-slate-950"
                    >
                      {phoneAction.icon}
                    </button>
                  )}
                  {topActions.map((action) => (
                    <button
                      key={action.id}
                      type="button"
                      data-draft-top-action={action.id}
                      onClick={action.onClick}
                      disabled={action.disabled}
                      aria-label={action.label}
                      title={action.label}
                      className={`relative z-10 flex min-h-11 shrink-0 items-center justify-center rounded-[8px] border px-2 text-[10px] font-semibold transition-colors disabled:cursor-not-allowed disabled:opacity-45 ${action.tone === "danger"
                        ? "border-rose-400/35 bg-rose-950/45 text-rose-200 hover:border-rose-300/55"
                        : action.tone === "emerald"
                          ? "border-emerald-400/35 bg-emerald-950/45 text-emerald-200 hover:border-emerald-300/55"
                          : "border-hairline bg-black/45 text-fg-muted hover:border-white/15 hover:bg-slate-950"}`}
                    >
                      {action.label}
                    </button>
                  ))}
                </>
              )}
              {responsiveDraftChrome && showProgress ? (
                <div
                  data-shell-draft-steps
                  className="pointer-events-none absolute inset-x-0 z-0 flex min-w-0 justify-center px-2"
                >
                  <DraftSteps
                    phase={shellDraftPhase}
                    compact
                    arrowSeparators={phoneDraftChrome}
                    flow={draftChromeConfig.progressVariant}
                  />
                </div>
              ) : !responsiveDraftChrome ? (
                <SocialBar />
              ) : null}
            </div>
            {/* Inner Suspense so a lazy route's load swaps ONLY the content area —
                the rail/scene persist (true SPA feel). */}
            <main className={`shell-content min-h-0 min-w-0 flex-1 ${responsiveDraftChrome ? "overflow-hidden" : "max-[820px]:pb-[76px]"}`}>
              {/* Above the Suspense boundary, not inside it: a banner mounted
                  inside would be swapped out for the route spinner on every
                  lazy-chunk load and blink away on each navigation. */}
              {showStatusBanner && <StatusBanner />}
              <Suspense
                fallback={
                  <div className="flex min-h-full items-center justify-center py-24">
                    <div className="h-8 w-8 animate-spin rounded-full border-2 border-slate-600 border-t-white" />
                  </div>
                }
              >
                <Outlet />
              </Suspense>
            </main>
          </div>
        </div>

        {!phoneDraftChrome && <TabBar onWhatsNew={openWhatsNew} hasUnread={changelog.hasUnread} />}
        <ChromeControls
          settingsOpen={settingsOpen}
          onSettingsOpenChange={setSettingsOpen}
          settingsReturnFocusRef={settingsReturnFocusRef}
          hideVolume={phoneDraftChrome}
          hideLanguage={phoneDraftChrome}
        />

        {whatsNewOpen && (
          <WhatsNewModal
            entries={changelog.entries}
            loading={changelog.loading}
            failed={changelog.failed}
            onRetry={changelog.openAndLoad}
            onClose={() => setWhatsNewOpen(false)}
          />
        )}
        </div>
      </DraftShellChromeProvider>
    </ShellProvider>
  );
}
