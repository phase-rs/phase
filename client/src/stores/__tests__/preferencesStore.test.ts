import { act } from "react";
import { beforeEach, describe, expect, it } from "vitest";

import { usePreferencesStore } from "../preferencesStore";

describe("preferencesStore", () => {
  beforeEach(() => {
    // Reset store state between tests
    act(() => {
      usePreferencesStore.setState({
        cardSize: "medium",
        hudLayout: "inline",
        followActiveOpponent: false,
        logPanelLastChoice: "closed",
        logDockSide: "right",
        boardBackground: "auto-wubrg",
        vfxQuality: "full",
        animationSpeedMultiplier: 1.0,
        showCardPreviewFooter: true,
        draftDoubleClickConfirmPick: true,
        pacingMultipliers: { effects: 1.0, combat: 1.0, banners: 1.0 },
        priorityPassingMode: "Standard",
        masterVolume: 100,
        sfxVolume: 70,
        musicVolume: 40,
        sfxMuted: false,
        musicMuted: false,
        masterMuted: false,
        multiplayerBoardLayout: "auto",
        multiplayerSplitLayoutNudgeDismissed: true,
        aiSeats: [{ difficulty: "Medium", deckId: "Random" }],
        aiBracketFilter: [],
      });
    });
    localStorage.clear();
  });

  it("has correct default values", () => {
    const state = usePreferencesStore.getState();

    expect(state.cardSize).toBe("medium");
    expect(state.hudLayout).toBe("inline");
    expect(state.followActiveOpponent).toBe(false);
    expect(usePreferencesStore.getInitialState().logPanelLastChoice).toBe("open");
    expect(usePreferencesStore.getInitialState().logDockSide).toBe("right");
    expect(state.boardBackground).toBe("auto-wubrg");
    // Read the store's real initialization snapshot (the getInitialState idiom
    // used below): the shared beforeEach writes its own defaults snapshot, so a
    // getState() read here would assert that snapshot, not buildDefaultPreferences().
    expect(usePreferencesStore.getInitialState().multiplayerBoardLayout).toBe("auto");
    expect(usePreferencesStore.getInitialState().multiplayerSplitLayoutNudgeDismissed).toBe(true);
    expect(state.aiSeats).toEqual([{ difficulty: "Medium", deckId: "Random" }]);
    expect(state.priorityPassingMode).toBe("Standard");
    expect(state.draftDoubleClickConfirmPick).toBe(true);
  });

  it("setAiSeatDifficulty updates the target seat", () => {
    act(() => {
      usePreferencesStore.getState().ensureAiSeatCount(3);
      usePreferencesStore.getState().setAiSeatDifficulty(1, "Hard");
    });

    const seats = usePreferencesStore.getState().aiSeats;
    expect(seats).toHaveLength(3);
    expect(seats[0].difficulty).toBe("Medium");
    expect(seats[1].difficulty).toBe("Hard");
  });

  it("ensureAiSeatCount seeds new seats from the first seat", () => {
    act(() => {
      usePreferencesStore.getState().setAiSeatDifficulty(0, "Hard");
      usePreferencesStore.getState().ensureAiSeatCount(3);
    });

    const seats = usePreferencesStore.getState().aiSeats;
    expect(seats.map((s) => s.difficulty)).toEqual(["Hard", "Hard", "Hard"]);
  });

  it("ensureAiSeatCount shrinks without losing leading seats", () => {
    act(() => {
      usePreferencesStore.getState().ensureAiSeatCount(4);
      usePreferencesStore.getState().setAiSeatDeckId(0, "saved:Dimir Control");
      usePreferencesStore.getState().ensureAiSeatCount(1);
    });

    const seats = usePreferencesStore.getState().aiSeats;
    expect(seats).toHaveLength(1);
    expect(seats[0].deckId).toBe("saved:Dimir Control");
  });

  it("setCardSize updates card size", () => {
    act(() => {
      usePreferencesStore.getState().setCardSize("large");
    });

    expect(usePreferencesStore.getState().cardSize).toBe("large");
  });

  it("setHudLayout updates hud layout", () => {
    act(() => {
      usePreferencesStore.getState().setHudLayout("floating");
    });

    expect(usePreferencesStore.getState().hudLayout).toBe("floating");
  });

  it("setMultiplayerBoardLayout updates multiplayer board layout", () => {
    act(() => {
      usePreferencesStore.getState().setMultiplayerBoardLayout("split");
    });

    expect(usePreferencesStore.getState().multiplayerBoardLayout).toBe("split");
  });

  it("updates the multiplayer split-layout nudge dismissal independently", () => {
    act(() => {
      usePreferencesStore.getState().setMultiplayerSplitLayoutNudgeDismissed(false);
    });

    expect(usePreferencesStore.getState().multiplayerSplitLayoutNudgeDismissed).toBe(false);
    expect(usePreferencesStore.getState().multiplayerBoardLayout).toBe("auto");
  });

  it("setFollowActiveOpponent updates the value", () => {
    act(() => {
      usePreferencesStore.getState().setFollowActiveOpponent(true);
    });

    expect(usePreferencesStore.getState().followActiveOpponent).toBe(true);
  });

  it("setLogPanelLastChoice updates the remembered log visibility", () => {
    act(() => {
      usePreferencesStore.getState().setLogPanelLastChoice("open");
    });

    expect(usePreferencesStore.getState().logPanelLastChoice).toBe("open");
  });

  it("persists the game-log dock side and reset restores the right dock", () => {
    act(() => {
      usePreferencesStore.getState().setLogDockSide("left");
    });

    expect(usePreferencesStore.getState().logDockSide).toBe("left");
    expect(JSON.parse(localStorage.getItem("phase-preferences")!).state.logDockSide).toBe("left");

    act(() => {
      usePreferencesStore.getState().resetAllPreferences();
    });

    expect(usePreferencesStore.getState().logDockSide).toBe("right");
  });

  it("setBoardBackground updates board background", () => {
    act(() => {
      usePreferencesStore.getState().setBoardBackground("blue");
    });

    expect(usePreferencesStore.getState().boardBackground).toBe("blue");
  });

  it("has correct default vfxQuality", () => {
    expect(usePreferencesStore.getState().vfxQuality).toBe("full");
  });

  it("has correct default animationSpeedMultiplier", () => {
    expect(usePreferencesStore.getState().animationSpeedMultiplier).toBe(1.0);
  });

  it("has correct default pacingMultipliers", () => {
    expect(usePreferencesStore.getState().pacingMultipliers).toEqual({
      effects: 1.0,
      combat: 1.0,
      banners: 1.0,
    });
  });

  it("setVfxQuality updates the value", () => {
    act(() => {
      usePreferencesStore.getState().setVfxQuality("minimal");
    });

    expect(usePreferencesStore.getState().vfxQuality).toBe("minimal");
  });

  it("shows the card preview footer by default and persists changes", () => {
    // Read the store's actual initialization snapshot so the shared beforeEach
    // reset cannot mask a regression in buildDefaultPreferences().
    expect(usePreferencesStore.getInitialState().showCardPreviewFooter).toBe(true);

    act(() => {
      usePreferencesStore.getState().setShowCardPreviewFooter(false);
    });

    expect(usePreferencesStore.getState().showCardPreviewFooter).toBe(false);
    const stored = JSON.parse(localStorage.getItem("phase-preferences")!);
    expect(stored.state.showCardPreviewFooter).toBe(false);
  });

  it("setAnimationSpeedMultiplier updates the value", () => {
    act(() => {
      usePreferencesStore.getState().setAnimationSpeedMultiplier(0.5);
    });

    expect(usePreferencesStore.getState().animationSpeedMultiplier).toBe(0.5);
  });

  it("setAnimationSpeedMultiplier clamps out-of-range values", () => {
    act(() => {
      usePreferencesStore.getState().setAnimationSpeedMultiplier(99);
    });
    expect(usePreferencesStore.getState().animationSpeedMultiplier).toBe(2);

    act(() => {
      usePreferencesStore.getState().setAnimationSpeedMultiplier(-5);
    });
    expect(usePreferencesStore.getState().animationSpeedMultiplier).toBe(0);
  });

  it("setPacingMultiplier updates a single category without disturbing others", () => {
    act(() => {
      usePreferencesStore.getState().setPacingMultiplier("combat", 1.75);
    });

    expect(usePreferencesStore.getState().pacingMultipliers).toEqual({
      effects: 1.0,
      combat: 1.75,
      banners: 1.0,
    });
  });

  it("setPacingMultiplier clamps to bounds", () => {
    act(() => {
      usePreferencesStore.getState().setPacingMultiplier("banners", 99);
    });
    expect(usePreferencesStore.getState().pacingMultipliers.banners).toBe(2);

    act(() => {
      usePreferencesStore.getState().setPacingMultiplier("effects", -5);
    });
    expect(usePreferencesStore.getState().pacingMultipliers.effects).toBe(0);
  });

  it("resetPacing returns animation speed and every category to 1.0", () => {
    act(() => {
      usePreferencesStore.getState().setAnimationSpeedMultiplier(0.25);
      usePreferencesStore.getState().setPacingMultiplier("combat", 1.75);
      usePreferencesStore.getState().setPacingMultiplier("banners", 0.5);
    });

    act(() => {
      usePreferencesStore.getState().resetPacing();
    });

    const state = usePreferencesStore.getState();
    expect(state.animationSpeedMultiplier).toBe(1.0);
    expect(state.pacingMultipliers).toEqual({ effects: 1.0, combat: 1.0, banners: 1.0 });
  });

  it("resetAllPreferences wipes everything back to defaults", () => {
    act(() => {
      usePreferencesStore.getState().setCardSize("large");
      usePreferencesStore.getState().setMasterVolume(20);
      usePreferencesStore.getState().setPacingMultiplier("combat", 1.5);
      usePreferencesStore.getState().setPriorityPassingMode("SkipLowUseWindows");
      usePreferencesStore.getState().setDraftDoubleClickConfirmPick(false);
    });

    act(() => {
      usePreferencesStore.getState().resetAllPreferences();
    });

    const state = usePreferencesStore.getState();
    expect(state.cardSize).toBe("medium");
    expect(state.masterVolume).toBe(100);
    expect(state.pacingMultipliers).toEqual({ effects: 1.0, combat: 1.0, banners: 1.0 });
    expect(state.priorityPassingMode).toBe("Standard");
    expect(state.draftDoubleClickConfirmPick).toBe(true);
  });

  it("existing preferences are unchanged after setting animation prefs", () => {
    act(() => {
      usePreferencesStore.getState().setVfxQuality("reduced");
      usePreferencesStore.getState().setAnimationSpeedMultiplier(1.5);
    });

    const state = usePreferencesStore.getState();
    expect(state.cardSize).toBe("medium");
    expect(state.hudLayout).toBe("inline");
    expect(state.logPanelLastChoice).toBe("closed");
    expect(state.boardBackground).toBe("auto-wubrg");
  });

  it("persists to localStorage with phase-preferences key", () => {
    act(() => {
      usePreferencesStore.getState().setCardSize("small");
      usePreferencesStore.getState().setFollowActiveOpponent(true);
      usePreferencesStore.getState().setAiSeatDifficulty(0, "VeryHard");
      usePreferencesStore.getState().setDraftDoubleClickConfirmPick(false);
    });

    // Zustand persist writes to localStorage
    const stored = localStorage.getItem("phase-preferences");
    expect(stored).toBeTruthy();

    const parsed = JSON.parse(stored!);
    expect(parsed.state.cardSize).toBe("small");
    expect(parsed.state.followActiveOpponent).toBe(true);
    expect(parsed.state.aiSeats[0].difficulty).toBe("VeryHard");
    expect(parsed.state.draftDoubleClickConfirmPick).toBe(false);
  });

  it("normalizes a current-version persisted locale before consumers can use it", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({ state: { language: "pt-BR" }, version: 34 }),
    );

    act(() => usePreferencesStore.persist.rehydrate());

    expect(usePreferencesStore.getState().language).toBe("pt");
  });

  it("falls back from an unsupported current-version persisted locale", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({ state: { language: "zh-Hans" }, version: 34 }),
    );

    act(() => usePreferencesStore.persist.rehydrate());

    expect(["en", "es", "fr", "de", "it", "pt", "pl"]).toContain(
      usePreferencesStore.getState().language,
    );
  });

  it("migrates pre-log-dock preferences to the prior right dock", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({ state: { logDockSide: "left" }, version: 32 }),
    );

    act(() => usePreferencesStore.persist.rehydrate());

    expect(usePreferencesStore.getState().logDockSide).toBe("right");
  });

  it("migrates a pre-v34 closed log default to open", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({ state: { logDefaultState: "closed" }, version: 33 }),
    );

    act(() => usePreferencesStore.persist.rehydrate());

    expect(usePreferencesStore.getState().logPanelLastChoice).toBe("open");
    expect(usePreferencesStore.getState()).not.toHaveProperty("logDefaultState");
  });

  it.each([undefined, "middle", 7])("resets an invalid current log-dock value (%j) to right", (logDockSide) => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({
        state: logDockSide === undefined ? {} : { logDockSide },
        version: 34,
      }),
    );

    act(() => usePreferencesStore.persist.rehydrate());

    expect(usePreferencesStore.getState().logDockSide).toBe("right");
  });

  it("migrates v1 enum animationSpeed='instant' to multiplier 0", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({
        state: {
          animationSpeed: "instant",
          combatPacing: "cinematic",
        },
        version: 1,
      }),
    );

    act(() => {
      usePreferencesStore.persist.rehydrate();
    });

    const state = usePreferencesStore.getState();
    // "instant" === 0 must survive the migration even though `0 || default`
    // would silently drop it.
    expect(state.animationSpeedMultiplier).toBe(0);
    expect(state.pacingMultipliers).toEqual({ effects: 1.0, combat: 1.75, banners: 1.0 });
  });

  it("migrates v2 combatPacingMultiplier into pacingMultipliers.combat", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({
        state: {
          animationSpeedMultiplier: 0.5,
          combatPacingMultiplier: 1.4,
        },
        version: 2,
      }),
    );

    act(() => {
      usePreferencesStore.persist.rehydrate();
    });

    const state = usePreferencesStore.getState();
    expect(state.animationSpeedMultiplier).toBe(0.5);
    expect(state.pacingMultipliers).toEqual({ effects: 1.0, combat: 1.4, banners: 1.0 });
    // The flat key must not leak through.
    expect((state as unknown as { combatPacingMultiplier?: unknown }).combatPacingMultiplier).toBeUndefined();
  });

  it("migrates legacy flat aiDifficulty/aiDeckName into aiSeats[0]", () => {
    // Simulate a v0 persisted blob (pre-multi-AI schema).
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({
        state: {
          aiDifficulty: "Hard",
          aiDeckName: "Dimir Control",
          cardSize: "large",
        },
        version: 0,
      }),
    );

    act(() => {
      usePreferencesStore.persist.rehydrate();
    });

    const state = usePreferencesStore.getState();
    expect(state.aiSeats).toEqual([{ difficulty: "Hard", deckId: "saved:Dimir Control" }]);
    expect(state.cardSize).toBe("large");
    // Legacy flat keys must not leak onto the state object.
    expect((state as unknown as { aiDifficulty?: unknown }).aiDifficulty).toBeUndefined();
    expect((state as unknown as { aiDeckName?: unknown }).aiDeckName).toBeUndefined();
  });

  it("migrates legacy AI seat deck names to catalog IDs while preserving Random", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({
        state: {
          aiSeats: [
            { difficulty: "Easy", deckName: "Random" },
            { difficulty: "Hard", deckName: "Dimir Control" },
          ],
        },
        version: 3,
      }),
    );

    act(() => {
      usePreferencesStore.persist.rehydrate();
    });

    expect(usePreferencesStore.getState().aiSeats).toEqual([
      { difficulty: "Easy", deckId: "Random" },
      { difficulty: "Hard", deckId: "saved:Dimir Control" },
    ]);
  });

  it("migrates v22 bare phaseStops into scoped { phase, scope: 'AllTurns' }", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({ state: { phaseStops: ["PreCombatMain"] }, version: 22 }),
    );

    act(() => {
      usePreferencesStore.persist.rehydrate();
    });

    expect(usePreferencesStore.getState().phaseStops).toEqual([
      { phase: "PreCombatMain", scope: "AllTurns" },
    ]);
  });

  it("migrates non-array v22 phaseStops to []", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({ state: { phaseStops: "garbage" }, version: 22 }),
    );

    act(() => {
      usePreferencesStore.persist.rehydrate();
    });

    expect(usePreferencesStore.getState().phaseStops).toEqual([]);
  });

  it.each([24, 25])("migrates the legacy Smart value from v%i", (version) => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({ state: { priorityPassingMode: "Smart" }, version }),
    );
    act(() => usePreferencesStore.persist.rehydrate());
    expect(usePreferencesStore.getState().priorityPassingMode).toBe("SkipLowUseWindows");
  });

  it("safely normalizes invalid v25 priority-passing modes", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({ state: { priorityPassingMode: "Aggressive" }, version: 25 }),
    );
    act(() => usePreferencesStore.persist.rehydrate());
    expect(usePreferencesStore.getState().priorityPassingMode).toBe("Standard");
  });

  // --- Audio preferences ---

  it("has correct audio defaults", () => {
    const state = usePreferencesStore.getState();

    expect(state.masterVolume).toBe(100);
    expect(state.sfxVolume).toBe(70);
    expect(state.musicVolume).toBe(40);
    expect(state.sfxMuted).toBe(false);
    expect(state.musicMuted).toBe(false);
    expect(state.masterMuted).toBe(false);
  });

  it("setMasterVolume updates master volume", () => {
    act(() => {
      usePreferencesStore.getState().setMasterVolume(65);
    });

    expect(usePreferencesStore.getState().masterVolume).toBe(65);
  });

  it("setSfxVolume updates sfx volume", () => {
    act(() => {
      usePreferencesStore.getState().setSfxVolume(50);
    });

    expect(usePreferencesStore.getState().sfxVolume).toBe(50);
  });

  it("setMusicVolume updates music volume", () => {
    act(() => {
      usePreferencesStore.getState().setMusicVolume(80);
    });

    expect(usePreferencesStore.getState().musicVolume).toBe(80);
  });

  it("setSfxMuted toggles sfx mute", () => {
    act(() => {
      usePreferencesStore.getState().setSfxMuted(true);
    });

    expect(usePreferencesStore.getState().sfxMuted).toBe(true);
  });

  it("setMusicMuted toggles music mute", () => {
    act(() => {
      usePreferencesStore.getState().setMusicMuted(true);
    });

    expect(usePreferencesStore.getState().musicMuted).toBe(true);
  });

  it("setMasterMuted toggles master mute", () => {
    act(() => {
      usePreferencesStore.getState().setMasterMuted(true);
    });

    expect(usePreferencesStore.getState().masterMuted).toBe(true);
  });

  it("audio preferences persist to localStorage", () => {
    act(() => {
      usePreferencesStore.getState().setSfxVolume(30);
    });

    const stored = localStorage.getItem("phase-preferences");
    expect(stored).toBeTruthy();

    const parsed = JSON.parse(stored!);
    expect(parsed.state.sfxVolume).toBe(30);
  });

  it("audio preferences don't affect existing preferences", () => {
    act(() => {
      usePreferencesStore.getState().setSfxVolume(30);
      usePreferencesStore.getState().setMusicVolume(90);
      usePreferencesStore.getState().setSfxMuted(true);
      usePreferencesStore.getState().setMusicMuted(true);
      usePreferencesStore.getState().setMasterMuted(true);
    });

    const state = usePreferencesStore.getState();
    expect(state.cardSize).toBe("medium");
    expect(state.hudLayout).toBe("inline");
  });

  it("hydrates from pre-populated localStorage", () => {
    // Pre-populate localStorage before store reads
    const stored = {
      state: {
        cardSize: "large",
        hudLayout: "floating",
        followActiveOpponent: true,
        boardBackground: "green",
      },
      version: 0,
    };
    localStorage.setItem("phase-preferences", JSON.stringify(stored));

    // Force rehydration
    act(() => {
      usePreferencesStore.persist.rehydrate();
    });

    const state = usePreferencesStore.getState();
    expect(state.cardSize).toBe("large");
    expect(state.hudLayout).toBe("floating");
    expect(state.followActiveOpponent).toBe(true);
    // `logPanelLastChoice` is deliberately NOT asserted here: the v33→v34 migration
    // rewrites it unconditionally, so on this legacy blob it would pass whatever
    // the seed said and would measure nothing. The real default is asserted via
    // getInitialState() above, and the migration itself has its own test.
    expect(state.boardBackground).toBe("green");
  });

  it("aiBracketFilter defaults to empty (filter off)", () => {
    const state = usePreferencesStore.getState();
    expect(state.aiBracketFilter).toEqual([]);
  });

  it("setAiBracketFilter replaces the array", () => {
    act(() => {
      usePreferencesStore.getState().setAiBracketFilter([2, 4]);
    });
    expect(usePreferencesStore.getState().aiBracketFilter).toEqual([2, 4]);

    act(() => {
      usePreferencesStore.getState().setAiBracketFilter([]);
    });
    expect(usePreferencesStore.getState().aiBracketFilter).toEqual([]);
  });

  it("v6 → v7 migration defaults aiBracketFilter to empty", () => {
    // Hydrate the persist key as a v6 payload (no aiBracketFilter field).
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({
        state: {
          aiSeats: [{ difficulty: "Medium", deckId: "Random" }],
          aiArchetypeFilter: "Any",
          aiCoverageFloor: 90,
        },
        version: 6,
      }),
    );

    // Force the store to re-hydrate so the migration runs.
    usePreferencesStore.persist.rehydrate();

    expect(usePreferencesStore.getState().aiBracketFilter).toEqual([]);
  });

  it("v6 → v7 migration replaces a non-array aiBracketFilter with empty", () => {
    // The legacy payload deliberately carries an invalid bracket value.
    // The migration's `Array.isArray` guard must reject it and substitute [].
    // If the migration code path is not exercised, this assertion fails
    // because the invalid value would survive the merge.
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({
        state: {
          aiSeats: [{ difficulty: "Medium", deckId: "Random" }],
          aiArchetypeFilter: "Any",
          aiCoverageFloor: 90,
          aiBracketFilter: "garbage",
        },
        version: 6,
      }),
    );

    usePreferencesStore.persist.rehydrate();

    expect(usePreferencesStore.getState().aiBracketFilter).toEqual([]);
  });

  it("v20 → v21 migration keeps pre-existing stores on the focused layout", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({
        state: {
          cardSize: "large",
        },
        version: 20,
      }),
    );

    act(() => {
      usePreferencesStore.persist.rehydrate();
    });

    expect(usePreferencesStore.getState().multiplayerBoardLayout).toBe("focused");
  });

  it("v29 → v30 migration defaults draft card previews to none", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({
        state: { cardPreviewMode: "follow" },
        version: 29,
      }),
    );

    act(() => {
      usePreferencesStore.persist.rehydrate();
    });

    expect(usePreferencesStore.getState().draftCardPreviewMode).toBe("none");
  });

  it("v30 → v31 migration enables draft double-click confirmation", () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({
        state: { draftCardPreviewMode: "none" },
        version: 30,
      }),
    );

    act(() => {
      usePreferencesStore.persist.rehydrate();
    });

    expect(usePreferencesStore.getState().draftDoubleClickConfirmPick).toBe(true);
  });
});
