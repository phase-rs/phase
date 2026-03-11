import { useCallback, useEffect, useRef, useState } from "react";
import { BrowserRouter, Routes, Route } from "react-router";

import { BuildBadge } from "./components/chrome/BuildBadge";
import { SplashScreen } from "./components/splash/SplashScreen";
import { MenuPage } from "./pages/MenuPage";
import { PlayPage } from "./pages/PlayPage";
import { MultiplayerPage } from "./pages/MultiplayerPage";
import { GamePage } from "./pages/GamePage";
import { GameSetupPage } from "./pages/GameSetupPage";
import { DeckBuilderPage } from "./pages/DeckBuilderPage";

export function App() {
  const [showSplash, setShowSplash] = useState(true);
  const [progress, setProgress] = useState(0);
  const rafRef = useRef<number>(0);
  const startRef = useRef<number>(0);

  // Simulate loading progress over ~1.5 seconds
  useEffect(() => {
    if (!showSplash) return;

    startRef.current = performance.now();

    function tick(now: number) {
      const elapsed = now - startRef.current;
      const pct = Math.min(100, (elapsed / 1500) * 100);
      setProgress(pct);
      if (pct < 100) {
        rafRef.current = requestAnimationFrame(tick);
      }
    }

    rafRef.current = requestAnimationFrame(tick);

    return () => cancelAnimationFrame(rafRef.current);
  }, [showSplash]);

  const handleSplashComplete = useCallback(() => {
    setShowSplash(false);
  }, []);

  return (
    <BrowserRouter>
      <div className="min-h-screen bg-gray-950 text-white">
        {showSplash && (
          <SplashScreen progress={progress} onComplete={handleSplashComplete} />
        )}
        <Routes>
          <Route path="/" element={<MenuPage />} />
          <Route path="/setup" element={<GameSetupPage />} />
          <Route path="/play" element={<PlayPage />} />
          <Route path="/multiplayer" element={<MultiplayerPage />} />
          <Route path="/deck-builder" element={<DeckBuilderPage />} />
          <Route path="/game/:id" element={<GamePage />} />
        </Routes>
        <BuildBadge />
      </div>
    </BrowserRouter>
  );
}
