/**
 * GameSetupPage — cEDH bracket warning chip tests.
 *
 * The warning chip renders when:
 *   activeDeckName !== null
 *   && cedhMode
 *   && !isDeckCedhLegal(humanDeckBracket)
 *
 * `activeDeckName` is read from localStorage (ACTIVE_DECK_KEY) on mount.
 * `humanDeckBracket` is read via `loadSavedDeckBracket(activeDeckName)`
 * which parses the stored deck JSON's `bracket` field from localStorage.
 * `aiSeats` lives in the preferences store.
 *
 * Heavy sub-components (MyDecks, MenuShell, MenuParticles, ScreenChrome,
 * AiOpponentConfig, FormatPicker, ModalPanelShell, useCardImage, audio,
 * WASM adapter, deckCompatibility, useBracketEstimate) are mocked so the
 * test only exercises the warning-chip render condition.
 */
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../../constants/storage";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { GameSetupPage } from "../GameSetupPage";

// ── Mock heavy sub-components and services ─────────────────────────────────

vi.mock("../../components/menu/MenuParticles", () => ({
  MenuParticles: () => null,
}));

vi.mock("../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

vi.mock("../../services/deckCompatibility", () => ({
  evaluateDeckCompatibilityBatch: vi.fn().mockResolvedValue({}),
}));

vi.mock("../../hooks/useBracketEstimate", () => ({
  useBracketEstimate: () => ({ estimate: null, loading: false, unsupported: false }),
}));

vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({ warmCardDatabase: () => Promise.resolve() }),
}));

vi.mock("../../audio/useAudioContext", () => ({
  useAudioContext: () => undefined,
}));

vi.mock("../../services/aiDeckCatalog", async () => {
  const actual =
    await vi.importActual<typeof import("../../services/aiDeckCatalog")>(
      "../../services/aiDeckCatalog",
    );
  return {
    ...actual,
    useAiDeckCatalog: () => ({ candidates: [], loading: false, error: null }),
  };
});

vi.mock("../../hooks/useSetSymbols", () => ({
  useSetSymbol: () => null,
}));

// MyDecks is a heavy component — stub it so tests focus on the warning chip.
vi.mock("../../components/menu/MyDecks", async () => {
  const actual = await vi.importActual<typeof import("../../components/menu/MyDecks")>(
    "../../components/menu/MyDecks",
  );
  return {
    ...actual,
    MyDecks: () => null,
  };
});

vi.mock("../../hooks/useDecks", async () => {
  const actual = await vi.importActual<typeof import("../../hooks/useDecks")>(
    "../../hooks/useDecks",
  );
  return {
    ...actual,
    useDecks: () => ({ decks: null, status: "success" as const }),
  };
});

// ── Helpers ────────────────────────────────────────────────────────────────

/** Write a minimal deck JSON into localStorage with an optional bracket tag. */
function seedDeck(deckName: string, bracket?: number): void {
  const data: Record<string, unknown> = {
    main: [{ name: "Island", count: 100 }],
    sideboard: [],
  };
  if (bracket !== undefined) {
    data.bracket = bracket;
  }
  localStorage.setItem(STORAGE_KEY_PREFIX + deckName, JSON.stringify(data));
}

/** Set the active deck in localStorage (simulates the user having selected a deck previously). */
function setActiveDeck(deckName: string): void {
  localStorage.setItem(ACTIVE_DECK_KEY, deckName);
}

function renderGameSetupPage(initialEntry = "/game-setup") {
  return render(
    <MemoryRouter initialEntries={[initialEntry]}>
      <Routes>
        <Route path="/game-setup" element={<GameSetupPage />} />
        <Route path="/" element={<div>Home</div>} />
        <Route path="/game/:id" element={<div>Game</div>} />
        <Route path="/deck-builder" element={<div>Deck Builder</div>} />
      </Routes>
    </MemoryRouter>,
  );
}

// ── Test suite ─────────────────────────────────────────────────────────────

beforeEach(() => {
  localStorage.clear();
  act(() => {
    // Reset to a single seat at Medium with cEDH mode off so each test starts
    // from a known state.
    usePreferencesStore.getState().ensureAiSeatCount(1);
    usePreferencesStore.getState().setAiSeatDifficulty(0, "Medium");
    usePreferencesStore.getState().setCedhMode(false);
    usePreferencesStore.setState({
      lastFormat: null,
      lastPlayerCount: 2,
      lastMatchType: "Bo1",
    });
  });
});

afterEach(cleanup);

describe("GameSetupPage — cEDH bracket warning chip", () => {
  it("offers implemented multiplayer variants in the format picker", async () => {
    const user = userEvent.setup();
    renderGameSetupPage();

    await user.click(await screen.findByRole("button", { name: /Commander/i }));

    expect(screen.getByText("Two-Headed Giant")).toBeInTheDocument();
    expect(screen.getByText("Planechase")).toBeInTheDocument();
    expect(screen.getByText("Archenemy")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Free-for-All/i })).toBeInTheDocument();
  });

  it("accepts a Two-Headed Giant URL format on the setup page", async () => {
    renderGameSetupPage("/game-setup?format=TwoHeadedGiant");

    expect(await screen.findByText("Two-Headed Giant")).toBeInTheDocument();
  });

  it("accepts Planechase on the setup page and explains the AI limitation", async () => {
    renderGameSetupPage("/game-setup?format=Planechase");

    expect(await screen.findByText("Planechase")).toBeInTheDocument();
    expect(screen.getByText(/AI matches are not supported/i)).toBeInTheDocument();
  });

  it("restores a Two-Headed Giant setup preference", async () => {
    act(() => {
      usePreferencesStore.setState({
        lastFormat: "TwoHeadedGiant",
        lastPlayerCount: 4,
        lastMatchType: "Bo3",
      });
    });

    renderGameSetupPage();

    expect(await screen.findByText("Two-Headed Giant")).toBeInTheDocument();
  });

  it("shows the warning chip when the human deck is non-cEDH and cEDH mode is on", async () => {
    // Deck with bracket 2 (Core) — not cEDH-legal.
    seedDeck("My Deck", 2);
    setActiveDeck("My Deck");

    act(() => {
      usePreferencesStore.getState().setCedhMode(true);
    });

    renderGameSetupPage();

    await waitFor(() => {
      const alert = screen.getByRole("alert");
      expect(alert).toBeInTheDocument();
      expect(alert).toHaveTextContent(/Your deck is bracket/i);
      expect(alert).toHaveTextContent(/vs\. a cEDH AI/i);
    });
  });

  it("does not show the warning chip when the human deck is cEDH-legal (bracket 5)", async () => {
    // Deck with bracket 5 — cEDH-legal.
    seedDeck("cEDH Deck", 5);
    setActiveDeck("cEDH Deck");

    act(() => {
      usePreferencesStore.getState().setCedhMode(true);
    });

    renderGameSetupPage();

    // Allow any async effects to settle.
    await waitFor(() => {
      // The deck name should appear (deck is loaded).
      expect(screen.getByText("cEDH Deck")).toBeInTheDocument();
    });

    // Warning must not be present.
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("does not show the warning chip when cEDH mode is off", async () => {
    // Deck with bracket 2 — would trigger the warning if cEDH mode were on.
    seedDeck("Casual Deck", 2);
    setActiveDeck("Casual Deck");

    // cEDH mode is off (reset in beforeEach); per-seat difficulty is irrelevant.
    act(() => {
      usePreferencesStore.getState().setCedhMode(false);
    });

    renderGameSetupPage();

    await waitFor(() => {
      expect(screen.getByText("Casual Deck")).toBeInTheDocument();
    });

    // No warning chip.
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
});
