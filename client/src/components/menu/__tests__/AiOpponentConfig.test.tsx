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
  });
});

afterEach(cleanup);

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
