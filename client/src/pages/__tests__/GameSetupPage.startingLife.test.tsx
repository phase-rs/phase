/**
 * GameSetupPage — the edited starting life must reach the started game.
 *
 * The game URL carries the format NAME only, so `GamePage` re-derives the
 * config from `FORMAT_DEFAULTS` unless the setup page hands the edited
 * `FormatConfig` over out-of-band. This suite pins the hand-over: the
 * active-game record (`ActiveGameMeta.formatConfig`) for the WASM route, and
 * router state for the native route, which deliberately writes no record.
 */
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes, useLocation } from "react-router";

import {
  ACTIVE_DECK_KEY,
  ACTIVE_GAME_KEY,
  STORAGE_KEY_PREFIX,
} from "../../constants/storage";
import { FORMAT_DEFAULTS } from "../../stores/multiplayerStore";
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
// The real component resolves that from the card database; report one so the
// gate opens without pulling the catalog in.
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

/** Echoes the router state the setup page navigated with. */
function GameRouteProbe() {
  const location = useLocation();
  return (
    <div data-testid="nav-state">
      {JSON.stringify((location.state as Record<string, unknown> | null) ?? null)}
    </div>
  );
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

beforeEach(() => {
  localStorage.clear();
  localStorage.setItem(
    STORAGE_KEY_PREFIX + "My Deck",
    JSON.stringify({ main: [{ name: "Island", count: 100 }], sideboard: [] }),
  );
  localStorage.setItem(ACTIVE_DECK_KEY, "My Deck");
});

afterEach(cleanup);

describe("GameSetupPage — starting life", () => {
  it("hands an edited starting life to the game it starts", async () => {
    const user = userEvent.setup();
    renderSetup();

    const lifeInput = await screen.findByLabelText("Starting Life");
    await user.clear(lifeInput);
    await user.type(lifeInput, "25");

    await user.click(screen.getByRole("button", { name: /Start Match/i }));

    const navState = JSON.parse(screen.getByTestId("nav-state").textContent!);
    expect(navState.formatConfig.starting_life).toBe(25);

    const meta = JSON.parse(localStorage.getItem(ACTIVE_GAME_KEY)!);
    expect(meta.formatConfig.starting_life).toBe(25);
  });

  it("keeps the last valid starting life when the field is left empty", async () => {
    const user = userEvent.setup();
    renderSetup();

    const lifeInput = await screen.findByLabelText("Starting Life");
    await user.clear(lifeInput);

    await user.click(screen.getByRole("button", { name: /Start Match/i }));

    const meta = JSON.parse(localStorage.getItem(ACTIVE_GAME_KEY)!);
    expect(meta.formatConfig.starting_life).toBe(
      FORMAT_DEFAULTS.Commander.starting_life,
    );
  });
});
