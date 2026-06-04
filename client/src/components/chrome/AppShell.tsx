import { Suspense, useState } from "react";
import { Outlet } from "react-router";

import { SceneParticles } from "../menu/MenuParticles";
import { BuildBadge } from "./BuildBadge";
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

  return (
    <ShellProvider value={true}>
      {/* The scene IS the relative root (matching how each page mounts it). NOTE:
          `.menu-scene` is unlayered CSS, which in Tailwind v4 outranks utilities,
          so it must not share an element with a conflicting position utility —
          keep it the relative container and let children position within it. The
          single scene here replaces every page's own (neutralized via
          `.shell-content .menu-scene` in index.css). */}
      <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
        <SceneParticles />
        <div className="menu-scene__vignette" />
        <div className="menu-scene__sigil menu-scene__sigil--left" />
        <div className="menu-scene__sigil menu-scene__sigil--right" />
        <div className="menu-scene__haze" />

        <CardDataLoadingBar />
        <Rail onSettings={() => setSettingsOpen(true)} />
        <SocialBar />

        {/* Reserve the top-left SocialBar's zone on every breakpoint so page
            titles (which sit at the top-left of each pane) clear the badge row:
            44px below the mobile strip, 56px below the desktop strip that aligns
            with ChromeControls at top:1rem. */}
        <main className="shell-content relative z-10 min-h-screen min-[820px]:ml-[92px] min-[820px]:pt-[calc(env(safe-area-inset-top)+56px)] max-[820px]:pt-[calc(env(safe-area-inset-top)+44px)] max-[820px]:pb-[76px]">
          {/* Inner Suspense so a lazy route's load swaps ONLY the content area —
              the rail/scene persist (true SPA feel). Without this, the App-level
              Suspense fallback would replace the whole shell, flashing like a
              full-page refresh on each first navigation to a route. */}
          <Suspense
            fallback={
              <div className="flex min-h-screen items-center justify-center">
                <div className="h-8 w-8 animate-spin rounded-full border-2 border-slate-600 border-t-white" />
              </div>
            }
          >
            <Outlet />
          </Suspense>
        </main>

        <TabBar />
        <ChromeControls
          settingsOpen={settingsOpen}
          onSettingsOpenChange={setSettingsOpen}
        />

        {/* The rail carries the version/update chip on desktop; below 820px the
            rail is hidden, so float it above the tab bar to keep the manual
            update check reachable on mobile/PWA. */}
        <div className="min-[820px]:hidden">
          <BuildBadge
            inline
            className="fixed bottom-[calc(env(safe-area-inset-bottom)+84px)] left-2 z-40"
          />
        </div>
      </div>
    </ShellProvider>
  );
}
