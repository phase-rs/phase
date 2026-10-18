import "../../test/helpers/persistedStorage";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { clearActiveGame, saveActiveGame } from "../../stores/gameStore.ts";
import { AI_DECK_RANDOM, usePreferencesStore } from "../../stores/preferencesStore.ts";
import { resolveLocalAiSeatPlan } from "../localAiSeatPlan.ts";

const GAME_ID = "local-ai-game";

function configurePreferences(options: {
  cedhMode?: boolean;
  seats: { difficulty: "Easy" | "Medium" | "Hard"; deckId: string }[];
}) {
  const prefs = usePreferencesStore.getState();
  prefs.setCedhMode(options.cedhMode ?? false);
  prefs.ensureAiSeatCount(options.seats.length);
  options.seats.forEach((seat, index) => {
    prefs.setAiSeatDifficulty(index, seat.difficulty);
    prefs.setAiSeatDeckId(index, seat.deckId);
  });
}

describe("resolveLocalAiSeatPlan", () => {
  beforeEach(() => {
    clearActiveGame();
    configurePreferences({
      seats: [
        { difficulty: "Easy", deckId: "Pref Deck 0" },
        { difficulty: "Hard", deckId: AI_DECK_RANDOM },
      ],
    });
  });

  afterEach(() => {
    clearActiveGame();
    usePreferencesStore.getState().setCedhMode(false);
  });

  it("falls back to the single-player preferences when no snapshot exists", () => {
    expect(resolveLocalAiSeatPlan(GAME_ID, 3)).toEqual([
      { difficulty: "Easy", deckId: "Pref Deck 0" },
      { difficulty: "Hard", deckId: AI_DECK_RANDOM },
    ]);
  });

  it("maps table cEDH into every seat's difficulty when reading preferences", () => {
    usePreferencesStore.getState().setCedhMode(true);
    expect(resolveLocalAiSeatPlan(GAME_ID, 3).map((seat) => seat.difficulty)).toEqual(["CEDH", "CEDH"]);
  });

  it("lets the game's seat snapshot win over preferences", () => {
    saveActiveGame({
      id: GAME_ID,
      mode: "ai",
      difficulty: "VeryHard",
      aiSeats: [
        { difficulty: "VeryHard", deckId: "Host Deck 0" },
        { difficulty: "Medium", deckId: "Host Deck 1" },
      ],
    });
    expect(resolveLocalAiSeatPlan(GAME_ID, 3)).toEqual([
      { difficulty: "VeryHard", deckId: "Host Deck 0" },
      { difficulty: "Medium", deckId: "Host Deck 1" },
    ]);
  });

  it("reads a snapshot deckId of null as Random, not the preference deck", () => {
    saveActiveGame({
      id: GAME_ID,
      mode: "ai",
      difficulty: "Medium",
      aiSeats: [{ difficulty: "Medium", deckId: null }, { difficulty: "Medium", deckId: null }],
    });
    expect(resolveLocalAiSeatPlan(GAME_ID, 3).map((seat) => seat.deckId)).toEqual([
      AI_DECK_RANDOM,
      AI_DECK_RANDOM,
    ]);
  });

  it("falls back to the preference deck for a snapshot seat without a deckId", () => {
    saveActiveGame({
      id: GAME_ID,
      mode: "ai",
      difficulty: "Medium",
      aiSeats: [{ difficulty: "Medium" }, { difficulty: "Medium" }],
    });
    expect(resolveLocalAiSeatPlan(GAME_ID, 3)).toEqual([
      { difficulty: "Medium", deckId: "Pref Deck 0" },
      { difficulty: "Medium", deckId: AI_DECK_RANDOM },
    ]);
  });

  it("ignores a snapshot that belongs to another game", () => {
    saveActiveGame({
      id: "some-other-game",
      mode: "ai",
      difficulty: "VeryHard",
      aiSeats: [{ difficulty: "VeryHard", deckId: "Other Deck" }],
    });
    expect(resolveLocalAiSeatPlan(GAME_ID, 2)).toEqual([
      { difficulty: "Easy", deckId: "Pref Deck 0" },
    ]);
  });

  it("defaults seats neither source configures to Random, never to seat 0", () => {
    expect(resolveLocalAiSeatPlan(GAME_ID, 5)).toEqual([
      { difficulty: "Easy", deckId: "Pref Deck 0" },
      { difficulty: "Hard", deckId: AI_DECK_RANDOM },
      { difficulty: "Medium", deckId: AI_DECK_RANDOM },
      { difficulty: "Medium", deckId: AI_DECK_RANDOM },
    ]);
  });

  it("always plans at least one opponent seat", () => {
    expect(resolveLocalAiSeatPlan(GAME_ID, 1)).toHaveLength(1);
  });
});
