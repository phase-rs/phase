import { lazy, Suspense, useCallback, useEffect, useState } from "react";
import { BrowserRouter, Routes, Route, useLocation } from "react-router";

import { BuildBadge } from "./components/chrome/BuildBadge";
import { HostControlTile } from "./components/chrome/HostControlTile";
import { EngineLostModal } from "./components/modal/EngineLostModal";
import { NonFatalPanicToast } from "./components/modal/NonFatalPanicToast";
import { SplashScreen } from "./components/splash/SplashScreen";
import { useFeedInitialization } from "./hooks/useFeedInitialization";
import { useHostingSession } from "./hooks/useHostingSession";
import { ensurePreload, subscribePreload } from "./startup/preloadAssets";
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

export function App() {
  return (
    <BrowserRouter unstable_useTransitions={false}>
      <AppContent />
    </BrowserRouter>
  );
}

function AppContent() {
  useFeedInitialization();
  useHostingSession();

  const [showSplash, setShowSplash] = useState(true);
  const [progress, setProgress] = useState(0);
  const [loadLabel, setLoadLabel] = useState("Loading...");
  const location = useLocation();

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
          <Route path="/" element={<MenuPage />} />
          <Route path="/setup" element={<GameSetupPage />} />
          <Route path="/multiplayer" element={<MultiplayerPage />} />
          <Route path="/my-decks" element={<MyDecksPage />} />
          <Route path="/deck-builder" element={<DeckBuilderPage />} />
          <Route path="/coverage" element={<CoveragePage />} />
          <Route path="/draft" element={<DraftLandingPage />} />
          <Route path="/draft/quick" element={<DraftPage />} />
          <Route path="/draft-pod" element={<DraftPodPage />} />
          <Route path="/game/:id" element={<GamePage />} />
        </Routes>
      </Suspense>
      {!location.pathname.startsWith("/game/") && <BuildBadge />}
      <HostControlTile />
      <EngineLostModal />
      <NonFatalPanicToast />
    </div>
  );
}
