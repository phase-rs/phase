import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import {
  ACTIVE_DECK_KEY,
  RANDOM_DECK_SELECTION,
  STORAGE_KEY_PREFIX,
} from "../../constants/storage";
import { GameSetupPage } from "../GameSetupPage";

const { useCardImage, warm } = vi.hoisted(() => ({
  useCardImage: vi.fn(),
  warm: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../../hooks/useCardImage", () => ({ useCardImage }));
vi.mock("../../components/menu/MenuParticles", () => ({ MenuParticles: () => null }));
vi.mock("../../audio/useAudioContext", () => ({ useAudioContext: () => undefined }));
vi.mock("../../components/menu/MyDecks", async () => {
  const actual = await vi.importActual<typeof import("../../components/menu/MyDecks")>(
    "../../components/menu/MyDecks",
  );
  return { ...actual, MyDecks: () => null };
});
vi.mock("../../hooks/useDecks", async () => {
  const actual = await vi.importActual<typeof import("../../hooks/useDecks")>("../../hooks/useDecks");
  return { ...actual, useDecks: () => ({ decks: null, status: "success" as const }) };
});
vi.mock("../../services/aiDeckCatalog", async () => {
  const actual = await vi.importActual<typeof import("../../services/aiDeckCatalog")>(
    "../../services/aiDeckCatalog",
  );
  return {
    ...actual,
    useAiDeckCatalog: () => ({ candidates: [], loading: false, error: null }),
  };
});
vi.mock("../../stores/cardDataStore", () => {
  const store = (selector: (state: { status: string }) => unknown) => selector({ status: "ready" });
  store.getState = () => ({ warm });
  return { useCardDataStore: store };
});
vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({ warmCardDatabase: () => Promise.resolve() }),
}));

function renderSetup() {
  return render(
    <MemoryRouter initialEntries={["/game-setup"]}>
      <Routes>
        <Route path="/game-setup" element={<GameSetupPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("GameSetupPage visual packs", () => {
  beforeEach(() => {
    localStorage.clear();
    useCardImage.mockReset();
    useCardImage.mockReturnValue({ src: null, isLoading: false });
  });

  afterEach(cleanup);

  it("passes saved printing identity and advances the installed setup cover", async () => {
    const advanceFailedSource = vi.fn();
    useCardImage.mockReturnValue({
      src: "visual-pack://installed/setup-cover",
      isLoading: false,
      advanceFailedSource,
    });
    localStorage.setItem(ACTIVE_DECK_KEY, "Printed Deck");
    localStorage.setItem(`${STORAGE_KEY_PREFIX}Printed Deck`, JSON.stringify({
      main: [{
        name: "Opt",
        count: 4,
        sourcePrinting: { setCode: "DAR", collectorNumber: "60" },
      }],
      sideboard: [],
    }));

    renderSetup();

    expect(await screen.findByText("Printed Deck")).toBeInTheDocument();
    expect(useCardImage).toHaveBeenCalledWith("Opt", {
      size: "art_crop",
      sourcePrinting: { setCode: "DAR", collectorNumber: "60" },
    });
    const image = document.querySelector('img[src="visual-pack://installed/setup-cover"]');
    expect(image).not.toBeNull();
    fireEvent.error(image!);
    expect(advanceFailedSource).toHaveBeenCalledWith("visual-pack://installed/setup-cover");
  });

  it("does not fabricate printing identity for random selection", async () => {
    localStorage.setItem(ACTIVE_DECK_KEY, RANDOM_DECK_SELECTION);

    renderSetup();

    await waitFor(() => {
      expect(useCardImage).toHaveBeenCalledWith("", {
        size: "art_crop",
        sourcePrinting: undefined,
      });
    });
  });
});
