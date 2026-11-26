/**
 * GameSetupPage — the picked archenemy seat must reach the started game.
 *
 * Solo Archenemy seats the human at seat 0 with AI opponents on seats 1..N.
 * The selector offers every seat (default: the human, preserving the
 * previous always-host behavior); the pick rides the `FormatConfig` to the
 * game via router state and the active-game record, like the other config
 * edits. A pick stranded by shrinking the table clamps to the last (AI)
 * seat rather than silently returning the role to the player.
 */
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes, useLocation } from "react-router";

import {
  ACTIVE_DECK_KEY,
  ACTIVE_GAME_KEY,
  STORAGE_KEY_PREFIX,
} from "../../constants/storage";
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

function renderSetup(path = "/game-setup") {
  return render(
    <MemoryRouter initialEntries={[path]}>
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

describe("GameSetupPage — archenemy seat", () => {
  it("offers every seat for Archenemy, defaulting to the player", async () => {
    renderSetup("/game-setup?format=Archenemy");

    const you = await screen.findByRole("button", { name: "You" });
    const opp1 = screen.getByRole("button", { name: "Opponent 1" });
    expect(you).toHaveClass("bg-indigo-600");
    expect(opp1).not.toHaveClass("bg-indigo-600");
  });

  it("offers every seat for Archenemy Commander", async () => {
    renderSetup("/game-setup?format=ArchenemyCommander");

    expect(await screen.findByRole("button", { name: "Opponent 2" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "You" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Opponent 1" })).toBeInTheDocument();
  });

  it("offers no archenemy selector outside the Archenemy family", async () => {
    renderSetup("/game-setup?format=Commander");

    expect(await screen.findByLabelText("Starting Life")).toBeInTheDocument();
    expect(screen.queryByText("Archenemy")).not.toBeInTheDocument();
  });

  it("hands the picked AI archenemy seat to the game it starts", async () => {
    const user = userEvent.setup();
    renderSetup("/game-setup?format=Archenemy");

    await user.click(await screen.findByRole("button", { name: "Opponent 1" }));
    await user.click(screen.getByRole("button", { name: /Start Match/i }));

    const navState = JSON.parse(screen.getByTestId("nav-state").textContent!);
    expect(navState.formatConfig.archenemy_player).toBe(1);

    const meta = JSON.parse(localStorage.getItem(ACTIVE_GAME_KEY)!);
    expect(meta.formatConfig.archenemy_player).toBe(1);
  });

  it("starts the player as archenemy when no seat is picked", async () => {
    const user = userEvent.setup();
    renderSetup("/game-setup?format=Archenemy");

    expect(await screen.findByRole("button", { name: "You" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /Start Match/i }));

    const navState = JSON.parse(screen.getByTestId("nav-state").textContent!);
    expect(navState.formatConfig.archenemy_player).toBe(0);
  });

  it("clamps a stranded pick to the last AI seat when the table shrinks", async () => {
    const user = userEvent.setup();
    renderSetup("/game-setup?format=Archenemy");

    // Grow to five seats and hand the role to the last AI seat.
    const playerSlider = () =>
      screen.getByRole("slider", { name: /players/i }) as HTMLInputElement;
    fireEvent.change(await screen.findByRole("slider", { name: /players/i }), {
      target: { value: "5" },
    });
    await user.click(await screen.findByRole("button", { name: "Opponent 4" }));

    // Shrink below the pick: the role follows to the last remaining AI
    // seat rather than silently returning to the player.
    fireEvent.change(playerSlider(), { target: { value: "3" } });
    expect(await screen.findByRole("button", { name: "Opponent 2" })).toHaveClass("bg-indigo-600");
    expect(screen.queryByRole("button", { name: "Opponent 4" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Start Match/i }));
    const navState = JSON.parse(screen.getByTestId("nav-state").textContent!);
    expect(navState.formatConfig.archenemy_player).toBe(2);
  });
});
