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
        logDefaultState: "closed",
        boardBackground: "auto-wubrg",
        vfxQuality: "full",
        animationSpeedMultiplier: 1.0,
        pacingMultipliers: { effects: 1.0, combat: 1.0, banners: 1.0 },
        masterVolume: 100,
        sfxVolume: 70,
        musicVolume: 40,
        sfxMuted: false,
        musicMuted: false,
        masterMuted: false,
        aiSeats: [{ difficulty: "Medium", deckName: "Random" }],
      });
    });
    localStorage.clear();
  });

  it("has correct default values", () => {
    const state = usePreferencesStore.getState();

    expect(state.cardSize).toBe("medium");
    expect(state.hudLayout).toBe("inline");
    expect(state.followActiveOpponent).toBe(false);
    expect(state.logDefaultState).toBe("closed");
    expect(state.boardBackground).toBe("auto-wubrg");
    expect(state.aiSeats).toEqual([{ difficulty: "Medium", deckName: "Random" }]);
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
      usePreferencesStore.getState().setAiSeatDeckName(0, "Dimir Control");
      usePreferencesStore.getState().ensureAiSeatCount(1);
    });

    const seats = usePreferencesStore.getState().aiSeats;
    expect(seats).toHaveLength(1);
    expect(seats[0].deckName).toBe("Dimir Control");
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

  it("setFollowActiveOpponent updates the value", () => {
    act(() => {
      usePreferencesStore.getState().setFollowActiveOpponent(true);
    });

    expect(usePreferencesStore.getState().followActiveOpponent).toBe(true);
  });

  it("setLogDefaultState updates log default state", () => {
    act(() => {
      usePreferencesStore.getState().setLogDefaultState("open");
    });

    expect(usePreferencesStore.getState().logDefaultState).toBe("open");
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
    });

    act(() => {
      usePreferencesStore.getState().resetAllPreferences();
    });

    const state = usePreferencesStore.getState();
    expect(state.cardSize).toBe("medium");
    expect(state.masterVolume).toBe(100);
    expect(state.pacingMultipliers).toEqual({ effects: 1.0, combat: 1.0, banners: 1.0 });
  });

  it("existing preferences are unchanged after setting animation prefs", () => {
    act(() => {
      usePreferencesStore.getState().setVfxQuality("reduced");
      usePreferencesStore.getState().setAnimationSpeedMultiplier(1.5);
    });

    const state = usePreferencesStore.getState();
    expect(state.cardSize).toBe("medium");
    expect(state.hudLayout).toBe("inline");
    expect(state.logDefaultState).toBe("closed");
    expect(state.boardBackground).toBe("auto-wubrg");
  });

  it("persists to localStorage with phase-preferences key", () => {
    act(() => {
      usePreferencesStore.getState().setCardSize("small");
      usePreferencesStore.getState().setFollowActiveOpponent(true);
      usePreferencesStore.getState().setAiSeatDifficulty(0, "VeryHard");
    });

    // Zustand persist writes to localStorage
    const stored = localStorage.getItem("phase-preferences");
    expect(stored).toBeTruthy();

    const parsed = JSON.parse(stored!);
    expect(parsed.state.cardSize).toBe("small");
    expect(parsed.state.followActiveOpponent).toBe(true);
    expect(parsed.state.aiSeats[0].difficulty).toBe("VeryHard");
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
    expect(state.aiSeats).toEqual([{ difficulty: "Hard", deckName: "Dimir Control" }]);
    expect(state.cardSize).toBe("large");
    // Legacy flat keys must not leak onto the state object.
    expect((state as unknown as { aiDifficulty?: unknown }).aiDifficulty).toBeUndefined();
    expect((state as unknown as { aiDeckName?: unknown }).aiDeckName).toBeUndefined();
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
        logDefaultState: "open",
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
    expect(state.logDefaultState).toBe("open");
    expect(state.boardBackground).toBe("green");
  });
});
