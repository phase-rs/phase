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
    // Reset to a single AI seat at Medium difficulty to avoid cascade state from a prior test.
    usePreferencesStore.getState().ensureAiSeatCount(1);
    usePreferencesStore.getState().setAiSeatDifficulty(0, "Medium");
  });
});

afterEach(cleanup);

describe("AiOpponentConfig — cEDH cascade", () => {
  it("cascades all AI seats to cEDH when one seat is set to cEDH", async () => {
    const user = userEvent.setup();

    // Seed: two AI seats — seat 0 at Easy, seat 1 at Hard.
    act(() => {
      usePreferencesStore.getState().ensureAiSeatCount(2);
      usePreferencesStore.getState().setAiSeatDifficulty(0, "Easy");
      usePreferencesStore.getState().setAiSeatDifficulty(1, "Hard");
    });

    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={2} />);

    // Expand the first seat panel (multi-AI starts collapsed).
    await user.click(screen.getByRole("button", { name: /Opponent 1/i }));

    // Select cEDH from seat 0's difficulty dropdown.
    const difficultySelects = screen.getAllByRole("combobox", { name: /Difficulty/i });
    await user.selectOptions(difficultySelects[0], "CEDH");

    // Both seats in the store should now be CEDH.
    await waitFor(() => {
      const seats = usePreferencesStore.getState().aiSeats;
      expect(seats[0].difficulty).toBe("CEDH");
      expect(seats[1].difficulty).toBe("CEDH");
    });
  });

  it("does not cascade on selecting a non-cEDH difficulty", async () => {
    const user = userEvent.setup();

    // Seed: two AI seats — seat 0 at Easy, seat 1 at Hard.
    act(() => {
      usePreferencesStore.getState().ensureAiSeatCount(2);
      usePreferencesStore.getState().setAiSeatDifficulty(0, "Easy");
      usePreferencesStore.getState().setAiSeatDifficulty(1, "Hard");
    });

    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={2} />);

    // Expand the first seat panel.
    await user.click(screen.getByRole("button", { name: /Opponent 1/i }));

    // Change seat 0 from Easy to Medium.
    const difficultySelects = screen.getAllByRole("combobox", { name: /Difficulty/i });
    await user.selectOptions(difficultySelects[0], "Medium");

    // Seat 1 must still be Hard — no cascade fired.
    await waitFor(() => {
      const seats = usePreferencesStore.getState().aiSeats;
      expect(seats[0].difficulty).toBe("Medium");
      expect(seats[1].difficulty).toBe("Hard");
    });
  });

  it("shows the cEDH cascade notice when cascading from a non-cEDH seat", async () => {
    const user = userEvent.setup();

    // Seed: two AI seats — both non-cEDH so the cascade fires on first selection.
    act(() => {
      usePreferencesStore.getState().ensureAiSeatCount(2);
      usePreferencesStore.getState().setAiSeatDifficulty(0, "Easy");
      usePreferencesStore.getState().setAiSeatDifficulty(1, "Hard");
    });

    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={2} />);

    // Expand seat 0 and select cEDH.
    await user.click(screen.getByRole("button", { name: /Opponent 1/i }));
    const difficultySelects = screen.getAllByRole("combobox", { name: /Difficulty/i });
    await user.selectOptions(difficultySelects[0], "CEDH");

    // The cascade notice should appear.
    await waitFor(() => {
      expect(screen.getByRole("status")).toHaveTextContent(/All AI opponents set to cEDH/i);
    });
  });
});

describe("AiOpponentConfig — B5 lock badge", () => {
  it("renders the B5 lock badge when the seat difficulty is CEDH", async () => {
    const user = userEvent.setup();

    act(() => {
      usePreferencesStore.getState().ensureAiSeatCount(1);
      usePreferencesStore.getState().setAiSeatDifficulty(0, "Medium");
    });

    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);

    // Badge should not appear before cEDH is selected.
    expect(screen.queryByLabelText("B5 lock")).not.toBeInTheDocument();

    // Select cEDH difficulty.
    const difficultySelect = screen.getByRole("combobox", { name: /Difficulty/i });
    await user.selectOptions(difficultySelect, "CEDH");

    await waitFor(() => {
      expect(screen.getByLabelText("B5 lock")).toBeInTheDocument();
    });
  });

  it("hides the B5 lock badge when the seat difficulty is not CEDH", () => {
    act(() => {
      usePreferencesStore.getState().ensureAiSeatCount(1);
      usePreferencesStore.getState().setAiSeatDifficulty(0, "Hard");
    });

    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);
    expect(screen.queryByLabelText("B5 lock")).not.toBeInTheDocument();
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
    expect(screen.getByRole("option", { name: /Random \(4\)/ })).toBeInTheDocument();
  });

  it("selecting brackets {2, 4} narrows the pool to those candidates and excludes untagged", async () => {
    const user = userEvent.setup();
    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);

    await user.click(screen.getByRole("button", { name: "2 Core" }));
    await user.click(screen.getByRole("button", { name: "4 Optimized" }));

    await waitFor(() => {
      expect(screen.getByRole("option", { name: /Random \(2\)/ })).toBeInTheDocument();
    });
  });
});
