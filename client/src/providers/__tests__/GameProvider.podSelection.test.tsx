import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AiDeckCandidate } from "../../services/aiDeckCatalog";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { loadActiveGame, saveActiveGame } from "../../services/gamePersistence";
import { buildLocalAiDeckList } from "../GameProvider";

const selectPod = vi.hoisted(() => vi.fn());
const buildLegalAiDeckCatalog = vi.hoisted(() => vi.fn());

vi.mock("../../services/podSelection", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../services/podSelection")>();
  return { ...actual, selectPod, podSelectionSeed: () => 17 };
});

vi.mock("../../services/aiDeckCatalog", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../services/aiDeckCatalog")>();
  return { ...actual, buildLegalAiDeckCatalog };
});

function candidate(id: string, bracket: 1 | 2 | 3 | 4 | 5 = 2): AiDeckCandidate {
  return {
    id,
    name: id,
    source: { type: "precon", deckId: id, code: "TST" },
    deck: { main: [{ name: `${id} Card`, count: 60 }], sideboard: [], commander: [`${id} Commander`] },
    knownFormat: "Commander",
    coveragePct: 100,
    archetype: "Midrange",
    bracket,
    bracketProvenance: "declared",
    bracketDataVersion: "test-1",
  };
}

const playerDeck = {
  main: [{ name: "Forest", count: 60 }],
  sideboard: [],
  commander: ["Player Commander"],
};
const t = ((key: string) => key) as never;

describe("GameProvider pod selection", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    buildLegalAiDeckCatalog.mockResolvedValue({
      candidates: [candidate("deck-a"), candidate("deck-b"), candidate("deck-c")],
    });
    selectPod.mockResolvedValue({
      kind: "assignment",
      assignment: {
        seats: [
          { seat_index: 0, candidate_id: "deck-a", tier: "core", provenance: "declared", difficulty: "Medium", color_identity: [] },
          { seat_index: 1, candidate_id: "deck-b", tier: "core", provenance: "declared", difficulty: "Medium", color_identity: [] },
        ],
        relaxations: [],
      },
    });
    usePreferencesStore.setState({
      aiSeats: [
        { difficulty: "Medium", deckId: "Random" },
        { difficulty: "Hard", deckId: "Random" },
      ],
      cedhMode: false,
      aiBracketFilter: [],
      aiArchetypeFilter: "Any",
      aiCoverageFloor: 80,
    });
  });

  it("buildLocalAiDeckList honours aiBracketFilter", async () => {
    usePreferencesStore.setState({ aiBracketFilter: [2] });
    await buildLocalAiDeckList(t, "game-1", playerDeck, 3, { format: "Commander" } as never);
    expect(selectPod).toHaveBeenCalledWith(
      expect.any(Array),
      expect.objectContaining({ allowed: ["core"], prefer: "core" }),
    );
  });

  it("cedhMode requests a hard gate over cedh only", async () => {
    usePreferencesStore.setState({ cedhMode: true });
    await buildLocalAiDeckList(t, "game-2", playerDeck, 3, { format: "Commander" } as never);
    expect(selectPod).toHaveBeenCalledWith(
      expect.any(Array),
      expect.objectContaining({ allowed: ["cedh"], prefer: "cedh", enforcement: "hard_gate" }),
    );
  });

  it("a pinned seat is passed as occupied and excluded from seats", async () => {
    usePreferencesStore.setState({
      aiSeats: [
        { difficulty: "Medium", deckId: "deck-a" },
        { difficulty: "Medium", deckId: "Random" },
      ],
    });
    selectPod.mockResolvedValueOnce({
      kind: "assignment",
      assignment: {
        seats: [{ seat_index: 0, candidate_id: "deck-b", tier: "core", provenance: "declared", difficulty: "Medium", color_identity: [] }],
        relaxations: [],
      },
    });
    await buildLocalAiDeckList(t, "game-3", playerDeck, 3, { format: "Commander" } as never);
    expect(selectPod).toHaveBeenCalledWith(
      expect.any(Array),
      expect.objectContaining({
        seats: 1,
        occupied: [{ deck_id: "deck-a", commander: ["deck-a Commander"] }],
      }),
    );
  });

  it("persists and reuses one exact pod seed for the game", async () => {
    saveActiveGame({ id: "game-seed", mode: "ai", difficulty: "Medium" });
    await buildLocalAiDeckList(t, "game-seed", playerDeck, 3, { format: "Commander" } as never);
    await buildLocalAiDeckList(t, "game-seed", playerDeck, 3, { format: "Commander" } as never);

    expect(loadActiveGame()?.podSeed).toBe(17);
    expect(selectPod.mock.calls[0][1].seed).toBe(17);
    expect(selectPod.mock.calls[1][1].seed).toBe(17);
  });
});
