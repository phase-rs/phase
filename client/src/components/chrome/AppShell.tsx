import { Suspense, useState } from "react";
import { Outlet } from "react-router";

import { useChangelog } from "../../hooks/useChangelog";
import { SceneParticles } from "../menu/MenuParticles";
import { WhatsNewModal } from "../modal/WhatsNewModal";
import { CardDataLoadingBar } from "./CardDataLoadingBar";
import { ChromeControls } from "./ChromeControls";
import { Rail } from "./Rail";
import { ShellProvider } from "./ShellContext";
import { SocialBar } from "./SocialBar";
import { TabBar } from "./TabBar";

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
  // The shell owns settings-modal state so the rail's Settings button and the
  // (controlled) ChromeControls cog share one PreferencesModal instance.
  const [settingsOpen, setSettingsOpen] = useState(false);

  // "What's New": the unread dot lives on the rail, the modal is shell-owned.
  const [whatsNewOpen, setWhatsNewOpen] = useState(false);
  const changelog = useChangelog();
  const openWhatsNew = () => {
    setWhatsNewOpen(true);
    changelog.openAndLoad();
  };

  return (
    <ShellProvider value={true}>
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
      <div className="menu-scene relative flex min-h-screen flex-col overflow-x-clip">
        <SceneParticles />
        <div className="menu-scene__vignette" />
        <div className="menu-scene__sigil menu-scene__sigil--left" />
        <div className="menu-scene__sigil menu-scene__sigil--right" />
        <div className="menu-scene__haze" />

        <CardDataLoadingBar />

        {/* Rail (≥820px) + body column. Both the rail and the top chrome row
            occupy real layout space (sticky), so page content can never slide
            under them — no ml/pt reserves, no z-index races for in-flow chrome. */}
        <div className="relative z-10 flex min-h-screen">
          <Rail
            onSettings={() => setSettingsOpen(true)}
            onWhatsNew={openWhatsNew}
            hasUnread={changelog.hasUnread}
          />
          <div className="flex min-w-0 flex-1 flex-col">
            {/* Sticky top chrome row: hosts the social strip and reserves the
                vertical band the fixed top-right ChromeControls occupy (44px
                mobile / 56px desktop), so page content clears both. */}
            <div className="sticky top-0 z-30 flex items-center px-2 pb-1 pt-[calc(env(safe-area-inset-top)+0.5rem)] min-h-[calc(env(safe-area-inset-top)+44px)] min-[820px]:px-4 min-[820px]:pt-[calc(env(safe-area-inset-top)+0.75rem)] min-[820px]:min-h-[calc(env(safe-area-inset-top)+56px)]">
              <SocialBar />
            </div>
            {/* Inner Suspense so a lazy route's load swaps ONLY the content area —
                the rail/scene persist (true SPA feel). */}
            <main className="shell-content min-w-0 flex-1 max-[820px]:pb-[76px]">
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

        <TabBar onWhatsNew={openWhatsNew} hasUnread={changelog.hasUnread} />
        <ChromeControls
          settingsOpen={settingsOpen}
          onSettingsOpenChange={setSettingsOpen}
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
    </ShellProvider>
  );
}
