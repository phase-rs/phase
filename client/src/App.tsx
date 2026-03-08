import { BrowserRouter, Routes, Route } from "react-router";
import { MenuPage } from "./pages/MenuPage";
import { GamePage } from "./pages/GamePage";
import { DeckBuilderPage } from "./pages/DeckBuilderPage";

export function App() {
  return (
    <BrowserRouter>
      <div className="min-h-screen bg-gray-950 text-white">
        <Routes>
          <Route path="/" element={<MenuPage />} />
          <Route path="/game" element={<GamePage />} />
          <Route path="/deck-builder" element={<DeckBuilderPage />} />
        </Routes>
      </div>
    </BrowserRouter>
  );
}
