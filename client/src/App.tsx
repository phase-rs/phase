import { lazy, StrictMode, Suspense, useCallback, useEffect, useState, type ReactNode } from "react";
import { BrowserRouter, Routes, Route, useSearchParams } from "react-router";

import { AppShell } from "./components/chrome/AppShell";
import { HostControlTile } from "./components/chrome/HostControlTile";
import { EngineLostModal } from "./components/modal/EngineLostModal";
import { NonFatalPanicToast } from "./components/modal/NonFatalPanicToast";
import { SplashScreen } from "./components/splash/SplashScreen";
import { useFeedInitialization } from "./hooks/useFeedInitialization";
import { useHostingSession } from "./hooks/useHostingSession";
import { migrateSavedDecks } from "./services/deckMigrations";
import { ensurePreload, subscribePreload } from "./startup/preloadAssets";
import { useCloudSyncStore } from "./stores/cloudSyncStore";
import { MenuPage } from "./pages/MenuPage";

const GamePage = lazy(() =>
  import("./pages/GamePage").then((m) => ({ default: m.GamePage })),
);
const GameSetupPage = lazy(() =>
  import("./pages/GameSetupPage").then((m) => ({ default: m.GameSetupPage })),
);
const MultiplayerPage = lazy(() => import("./pages/MultiplayerPage").then((m) => ({ default: m.MultiplayerPage })));
const DeckBuilderPage = lazy(() => import("./pages/DeckBuilderPage").then((m) => ({ default: m.DeckBuilderPage })));
const MyDecksPage = lazy(() => import("./pages/MyDecksPage").then((m) => ({ default: m.MyDecksPage })));
const CoveragePage = lazy(() => import("./pages/CoveragePage").then((m) => ({ default: m.CoveragePage })));
const DraftLandingPage = lazy(() => import("./pages/DraftLandingPage").then((m) => ({ default: m.DraftLandingPage })));
const DraftPage = lazy(() => import("./pages/DraftPage").then((m) => ({ default: m.DraftPage })));
const DraftPodPage = lazy(() => import("./pages/DraftPodPage").then((m) => ({ default: m.DraftPodPage })));
const DraftSpectatorPage = lazy(() =>
  import("./pages/DraftSpectatorPage").then((m) => ({ default: m.DraftSpectatorPage })),
);

function DevStrict({ children }: { children: ReactNode }) {
  if (!import.meta.env.DEV) return children;
  return <StrictMode>{children}</StrictMode>;
}

function GameRouteElement() {
  const [searchParams] = useSearchParams();
  const mode = searchParams.get("mode");
  const isP2PGame = mode === "p2p-host" || mode === "p2p-join";

  if (isP2PGame) return <GamePage />;
  return (
    <DevStrict>
      <GamePage />
    </DevStrict>
  );
}

export function App() {
  return (
    <BrowserRouter useTransitions={false}>
      <AppContent />
    </BrowserRouter>
  );
}

function AppContent() {
  useFeedInitialization();
  useHostingSession();

  // One-shot localStorage migrations. Must run before cloud-sync init so the
  // first sync sees the canonical (repaired) deck shapes and doesn't push a
  // tab full of "changed" decks that are byte-identical after repair.
  useEffect(() => {
    migrateSavedDecks();
  }, []);

  // Install the storage watcher, restore any cloud-sync session, and reconcile
  // on boot. init() returns an uninstaller so listeners are cleaned up on
  // unmount / hot reload rather than stacking.
  useEffect(() => useCloudSyncStore.getState().init(), []);

  const [showSplash, setShowSplash] = useState(true);
  const [progress, setProgress] = useState(0);
  const [loadLabel, setLoadLabel] = useState("Loading...");

  // Run startup preload for shell-safe assets only.
  useEffect(() => {
    if (!showSplash) return;

    const unsub = subscribePreload((p) => {
      setProgress(p.percent);
      if (p.phase === "audio") setLoadLabel("Loading audio...");
      else setLoadLabel("Ready");
    });
    ensurePreload();
    return unsub;
  }, [showSplash]);

  const handleSplashComplete = useCallback(() => {
    setShowSplash(false);
  }, []);

  return (
    <div className="min-h-screen bg-gray-950 text-white">
      {showSplash && (
        <SplashScreen progress={progress} onComplete={handleSplashComplete} label={loadLabel} />
      )}
      <Suspense fallback={<div className="flex min-h-screen items-center justify-center"><div className="h-8 w-8 animate-spin rounded-full border-2 border-gray-500 border-t-white" /></div>}>
        <Routes>
          {/* Modern app shell (rail + tab bar + single scene) wraps every
              out-of-match surface; /game/:id stays full-screen outside it. */}
          <Route element={<AppShell />}>
            <Route path="/" element={<DevStrict><MenuPage /></DevStrict>} />
            <Route path="/setup" element={<DevStrict><GameSetupPage /></DevStrict>} />
            <Route path="/multiplayer" element={<DevStrict><MultiplayerPage /></DevStrict>} />
            <Route path="/my-decks" element={<DevStrict><MyDecksPage /></DevStrict>} />
            <Route path="/deck-builder" element={<DevStrict><DeckBuilderPage /></DevStrict>} />
            <Route path="/coverage" element={<DevStrict><CoveragePage /></DevStrict>} />
            <Route path="/draft" element={<DevStrict><DraftLandingPage /></DevStrict>} />
            <Route path="/draft/quick" element={<DevStrict><DraftPage /></DevStrict>} />
            <Route path="/draft-pod" element={<DraftPodPage />} />
            <Route path="/draft-spectator" element={<DraftSpectatorPage />} />
          </Route>
          <Route path="/game/:id" element={<GameRouteElement />} />
        </Routes>
      </Suspense>
      <HostControlTile />
      <EngineLostModal />
      <NonFatalPanicToast />
    </div>
  );
}
