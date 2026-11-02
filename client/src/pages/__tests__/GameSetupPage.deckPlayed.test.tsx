/**
 * GameSetupPage — the deck-played timestamp write must not turn a busy saved-deck
 * library into a stalled or crashed start. `handleStart` fires it through
 * `withSavedDeckLibraryOrSkip(..., "run-unguarded")` and never awaits or catches
 * the result, so the write must never reject.
 */
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes, useLocation } from "react-router";

import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../../constants/storage";
import { setSavedDeckTxnLockWaitForTests, withSavedDeckLibrary } from "../../services/savedDeckTransaction";
import {
  installFifoWebLocks,
  resetSavedDeckLibraryForTests,
  uninstallWebLocks,
} from "../../test/helpers/webLocks";
import { GameSetupPage } from "../GameSetupPage";

vi.mock("../../components/menu/MenuParticles", () => ({ MenuParticles: () => null }));
vi.mock("../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));
vi.mock("../../audio/useAudioContext", () => ({ useAudioContext: () => undefined }));
vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({ warmCardDatabase: () => Promise.resolve() }),
}));
vi.mock("../../components/menu/MyDecks", async () => {
  const actual = await vi.importActual<typeof import("../../components/menu/MyDecks")>(
    "../../components/menu/MyDecks",
  );
  return { ...actual, MyDecks: () => null };
});
vi.mock("../../hooks/useDecks", async () => {
  const actual = await vi.importActual<typeof import("../../hooks/useDecks")>(
    "../../hooks/useDecks",
  );
  return { ...actual, useDecks: () => ({ decks: null, status: "success" as const }) };
});
vi.mock("../../services/deckCompatibility", () => ({
  evaluateDeckCompatibilityBatch: vi.fn().mockResolvedValue({}),
}));
vi.mock("../../hooks/useBracketEstimate", () => ({
  useBracketEstimate: () => ({ estimate: null, loading: false, unsupported: false }),
}));
vi.mock("../../hooks/useSetSymbols", () => ({ useSetSymbol: () => null }));
vi.mock("../../stores/cardDataStore", () => {
  const store = (selector: (state: { status: string }) => unknown) => selector({ status: "ready" });
  store.getState = () => ({ warm: vi.fn().mockResolvedValue(undefined) });
  return { useCardDataStore: store };
});
// The Start button is gated on the AI seats having at least one legal deck.
vi.mock("../../components/menu/AiOpponentConfig", () => ({
  AiOpponentConfig: ({
    onCandidateCountChange,
  }: {
    onCandidateCountChange: (count: number) => void;
  }) => {
    onCandidateCountChange(1);
    return null;
  },
}));

function GameRouteProbe() {
  const location = useLocation();
  return <div data-testid="nav-state">{JSON.stringify((location.state as unknown) ?? null)}</div>;
}

function renderSetup() {
  return render(
    <MemoryRouter initialEntries={["/game-setup"]}>
      <Routes>
        <Route path="/game-setup" element={<GameSetupPage />} />
        <Route path="/game/:id" element={<GameRouteProbe />} />
      </Routes>
    </MemoryRouter>,
  );
}

beforeEach(async () => {
  installFifoWebLocks();
  await resetSavedDeckLibraryForTests(); // clears localStorage; seed the deck after
  localStorage.setItem(
    STORAGE_KEY_PREFIX + "My Deck",
    JSON.stringify({ main: [{ name: "Island", count: 100 }], sideboard: [] }),
  );
  localStorage.setItem(ACTIVE_DECK_KEY, "My Deck");
});

afterEach(() => {
  uninstallWebLocks();
  cleanup();
});

describe("GameSetupPage — deck-played tracking under a busy saved-deck library", () => {
  it("starts the match immediately, without waiting on or rejecting from the deck-played write", async () => {
    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });
    setSavedDeckTxnLockWaitForTests(20);

    const user = userEvent.setup();
    renderSetup();
    await user.click(screen.getByRole("button", { name: /Start Match/i }));

    // The start navigates before the deck-played write's own lock wait could
    // possibly settle — it must not be awaited.
    expect(await screen.findByTestId("nav-state")).toBeInTheDocument();

    // Let the deck-played write's own lock-wait timeout (20ms, set above) fire while
    // this test is still running, so an unhandled rejection would surface here.
    await new Promise((resolve) => setTimeout(resolve, 60));

    releaseHolder();
    await holder;
    setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
  });
});
