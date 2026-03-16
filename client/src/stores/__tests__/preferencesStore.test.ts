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
        animationSpeed: "normal",
        combatPacing: "normal",
        masterVolume: 100,
        sfxVolume: 70,
        musicVolume: 40,
        sfxMuted: false,
        musicMuted: false,
        masterMuted: false,
        aiDifficulty: "Medium",
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
    expect(state.aiDifficulty).toBe("Medium");
  });

  it("setAiDifficulty updates AI difficulty", () => {
    act(() => {
      usePreferencesStore.getState().setAiDifficulty("Hard");
    });

    expect(usePreferencesStore.getState().aiDifficulty).toBe("Hard");
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

  it("has correct default animationSpeed", () => {
    expect(usePreferencesStore.getState().animationSpeed).toBe("normal");
  });

  it("has correct default combatPacing", () => {
    expect(usePreferencesStore.getState().combatPacing).toBe("normal");
  });

  it("setVfxQuality updates the value", () => {
    act(() => {
      usePreferencesStore.getState().setVfxQuality("minimal");
    });

    expect(usePreferencesStore.getState().vfxQuality).toBe("minimal");
  });

  it("setAnimationSpeed updates the value", () => {
    act(() => {
      usePreferencesStore.getState().setAnimationSpeed("fast");
    });

    expect(usePreferencesStore.getState().animationSpeed).toBe("fast");
  });

  it("setCombatPacing updates the value", () => {
    act(() => {
      usePreferencesStore.getState().setCombatPacing("cinematic");
    });

    expect(usePreferencesStore.getState().combatPacing).toBe("cinematic");
  });

  it("existing preferences are unchanged after setting animation prefs", () => {
    act(() => {
      usePreferencesStore.getState().setVfxQuality("reduced");
      usePreferencesStore.getState().setAnimationSpeed("slow");
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
      usePreferencesStore.getState().setAiDifficulty("VeryHard");
    });

    // Zustand persist writes to localStorage
    const stored = localStorage.getItem("phase-preferences");
    expect(stored).toBeTruthy();

    const parsed = JSON.parse(stored!);
    expect(parsed.state.cardSize).toBe("small");
    expect(parsed.state.followActiveOpponent).toBe(true);
    expect(parsed.state.aiDifficulty).toBe("VeryHard");
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
