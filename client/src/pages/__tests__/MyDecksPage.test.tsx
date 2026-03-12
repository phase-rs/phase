import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router";

import { STORAGE_KEY_PREFIX } from "../../constants/storage";
import { MyDecksPage } from "../MyDecksPage";

vi.mock("../../components/menu/MenuParticles", () => ({
  MenuParticles: () => null,
}));

vi.mock("../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

vi.mock("../../services/deckCompatibility", () => ({
  evaluateDeckCompatibilityBatch: vi.fn().mockResolvedValue({}),
}));

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="location">{location.pathname}{location.search}</div>;
}

function renderMyDecksPage() {
  return render(
    <MemoryRouter initialEntries={["/my-decks"]}>
      <LocationProbe />
      <Routes>
        <Route path="/my-decks" element={<MyDecksPage />} />
        <Route path="/deck-builder" element={<div>Deck Builder</div>} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("MyDecksPage", () => {
  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Test Deck",
      JSON.stringify({ main: [{ name: "Island", count: 60 }], sideboard: [] }),
    );
  });

  afterEach(() => {
    cleanup();
  });

  it("routes create entry point to deck builder", () => {
    renderMyDecksPage();

    fireEvent.click(screen.getByRole("button", { name: "Create New" }));
    expect(screen.getByTestId("location").textContent).toBe(
      "/deck-builder?create=1&returnTo=%2Fmy-decks",
    );
  });

  it("routes edit entry point to deck builder with selected deck", async () => {
    renderMyDecksPage();

    fireEvent.click(await screen.findByText("Test Deck"));
    expect(screen.getByTestId("location").textContent).toBe(
      "/deck-builder?deck=Test%20Deck&returnTo=%2Fmy-decks",
    );
  });
});
