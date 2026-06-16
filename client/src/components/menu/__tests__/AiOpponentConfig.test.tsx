import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { AiOpponentConfig } from "../AiOpponentConfig";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import type { AiDeckCandidate } from "../../../services/aiDeckCatalog";

vi.mock("../../../services/aiDeckCatalog", async () => {
  const actual = await vi.importActual<typeof import("../../../services/aiDeckCatalog")>(
    "../../../services/aiDeckCatalog",
  );
  return {
    ...actual,
    useAiDeckCatalog: () => ({ candidates: mockCandidates, loading: false, error: null }),
  };
});

let mockCandidates: AiDeckCandidate[] = [];

function candidate(id: string, bracket: AiDeckCandidate["bracket"]): AiDeckCandidate {
  return {
    id,
    name: id,
    source: { type: "precon", deckId: id, code: "TST" },
    deck: { main: [], sideboard: [] },
    coveragePct: 100,
    archetype: null,
    bracket,
  };
}

beforeEach(() => {
  mockCandidates = [
    candidate("Bracket1", 1),
    candidate("Bracket2", 2),
    candidate("Bracket4", 4),
    candidate("Untagged", null),
  ];
  act(() => {
    usePreferencesStore.getState().setAiBracketFilter([]);
    usePreferencesStore.getState().setAiArchetypeFilter("Any");
    usePreferencesStore.getState().setAiCoverageFloor(50);
    // Reset to a single AI seat at Medium difficulty with cEDH mode off so each
    // test starts from a known state.
    usePreferencesStore.getState().ensureAiSeatCount(1);
    usePreferencesStore.getState().setAiSeatDifficulty(0, "Medium");
    usePreferencesStore.getState().setCedhMode(false);
  });
});

afterEach(cleanup);

describe("AiOpponentConfig — cEDH toggle", () => {
  it("enabling cEDH mode sets the table flag without touching per-seat difficulties", async () => {
    const user = userEvent.setup();

    // Seed: two AI seats — seat 0 at Easy, seat 1 at Hard.
    act(() => {
      usePreferencesStore.getState().ensureAiSeatCount(2);
      usePreferencesStore.getState().setAiSeatDifficulty(0, "Easy");
      usePreferencesStore.getState().setAiSeatDifficulty(1, "Hard");
    });

    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={2} />);

    // Flip the table-wide cEDH toggle.
    await user.click(screen.getByRole("switch", { name: /cEDH mode/i }));

    // cEDH mode is on, but each seat's remembered difficulty is preserved
    // (no cascade) so turning it back off restores the per-seat choices.
    await waitFor(() => {
      expect(usePreferencesStore.getState().cedhMode).toBe(true);
      const seats = usePreferencesStore.getState().aiSeats;
      expect(seats[0].difficulty).toBe("Easy");
      expect(seats[1].difficulty).toBe("Hard");
    });
  });

  it("per-seat difficulty changes are independent (no cascade to other seats)", async () => {
    const user = userEvent.setup();

    // Seed: two AI seats — seat 0 at Easy, seat 1 at Hard.
    act(() => {
      usePreferencesStore.getState().ensureAiSeatCount(2);
      usePreferencesStore.getState().setAiSeatDifficulty(0, "Easy");
      usePreferencesStore.getState().setAiSeatDifficulty(1, "Hard");
    });

    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={2} />);

    // Expand the first seat panel and change only seat 0.
    await user.click(screen.getByRole("button", { name: /Opponent 1/i }));
    const difficultyTriggers = screen.getAllByRole("button", { name: /^Difficulty$/i });
    await user.click(difficultyTriggers[0]);
    await user.click(screen.getByRole("option", { name: /Medium/i }));

    // Seat 1 must still be Hard — changing one seat never affects another.
    await waitFor(() => {
      const seats = usePreferencesStore.getState().aiSeats;
      expect(seats[0].difficulty).toBe("Medium");
      expect(seats[1].difficulty).toBe("Hard");
    });
  });
});

describe("AiOpponentConfig — cEDH toggle format gating", () => {
  it("renders the toggle for Commander", () => {
    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);
    expect(screen.getByRole("switch", { name: /cEDH mode/i })).toBeInTheDocument();
  });

  it("renders the toggle for Duel Commander", () => {
    render(<AiOpponentConfig selectedFormat="DuelCommander" opponentCount={1} />);
    expect(screen.getByRole("switch", { name: /cEDH mode/i })).toBeInTheDocument();
  });

  it("hides the toggle for non-Commander formats", () => {
    render(<AiOpponentConfig selectedFormat="Standard" opponentCount={1} />);
    expect(screen.queryByRole("switch", { name: /cEDH mode/i })).not.toBeInTheDocument();
  });

  it("clears a stale cEDH flag when switching away from a Commander format", async () => {
    act(() => {
      usePreferencesStore.getState().setCedhMode(true);
    });

    const { rerender } = render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);
    expect(usePreferencesStore.getState().cedhMode).toBe(true);

    // Simulate the setup page re-passing a new format prop on dropdown change.
    rerender(<AiOpponentConfig selectedFormat="Standard" opponentCount={1} />);

    await waitFor(() => {
      expect(usePreferencesStore.getState().cedhMode).toBe(false);
    });
  });
});

describe("AiOpponentConfig — cEDH badge + disabled difficulty", () => {
  it("badges the seat and disables the difficulty dropdown when cEDH mode is on", async () => {
    const user = userEvent.setup();

    act(() => {
      usePreferencesStore.getState().ensureAiSeatCount(1);
      usePreferencesStore.getState().setAiSeatDifficulty(0, "Medium");
    });

    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);

    // Before enabling: no badge, dropdown enabled.
    expect(screen.queryByLabelText("cEDH")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^Difficulty$/i })).toBeEnabled();

    await user.click(screen.getByRole("switch", { name: /cEDH mode/i }));

    await waitFor(() => {
      expect(screen.getByLabelText("cEDH")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /^Difficulty$/i })).toBeDisabled();
    });
  });

  it("hides the cEDH badge when cEDH mode is off", () => {
    act(() => {
      usePreferencesStore.getState().ensureAiSeatCount(1);
      usePreferencesStore.getState().setAiSeatDifficulty(0, "Hard");
    });

    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);
    expect(screen.queryByLabelText("cEDH")).not.toBeInTheDocument();
  });

  it("clears cEDH mode when switching away from Commander-family formats", async () => {
    act(() => {
      usePreferencesStore.getState().setCedhMode(true);
    });

    const { rerender } = render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);
    rerender(<AiOpponentConfig selectedFormat="Standard" opponentCount={1} />);

    await waitFor(() => {
      expect(usePreferencesStore.getState().cedhMode).toBe(false);
    });
  });

  it("preserves cEDH mode when the selected format is not loaded", () => {
    act(() => {
      usePreferencesStore.getState().setCedhMode(true);
    });

    const { rerender } = render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);
    rerender(<AiOpponentConfig selectedFormat={undefined} opponentCount={1} />);
    expect(usePreferencesStore.getState().cedhMode).toBe(true);

    rerender(<AiOpponentConfig selectedFormat={null} opponentCount={1} />);
    expect(usePreferencesStore.getState().cedhMode).toBe(true);
  });
});

describe("AiOpponentConfig — bracket filter", () => {
  it("does not render the bracket chip row when format is not Commander", () => {
    render(<AiOpponentConfig selectedFormat="Standard" opponentCount={1} />);
    expect(screen.queryByRole("group", { name: "Bracket filter" })).not.toBeInTheDocument();
  });

  it("renders the bracket chip row when format is Commander", () => {
    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);
    expect(screen.getByRole("group", { name: "Bracket filter" })).toBeInTheDocument();
  });

  it("filter off (empty selection) keeps untagged candidates in the random pool", () => {
    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);
    expect(screen.getByRole("button", { name: /^Deck$/i })).toHaveTextContent(/Random \(4\)/);
  });

  it("selecting brackets {2, 4} narrows the pool to those candidates and excludes untagged", async () => {
    const user = userEvent.setup();
    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);

    await user.click(screen.getByRole("button", { name: "2 Core" }));
    await user.click(screen.getByRole("button", { name: "4 Optimized" }));

    await waitFor(() => {
      expect(screen.getByRole("button", { name: /^Deck$/i })).toHaveTextContent(/Random \(2\)/);
    });
  });
});
