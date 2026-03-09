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
        logDefaultState: "closed",
        boardBackground: "auto-wubrg",
        vfxQuality: "full",
        animationSpeed: "normal",
      });
    });
    localStorage.clear();
  });

  it("has correct default values", () => {
    const state = usePreferencesStore.getState();

    expect(state.cardSize).toBe("medium");
    expect(state.hudLayout).toBe("inline");
    expect(state.logDefaultState).toBe("closed");
    expect(state.boardBackground).toBe("auto-wubrg");
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

  it("persists to localStorage with forge-preferences key", () => {
    act(() => {
      usePreferencesStore.getState().setCardSize("small");
    });

    // Zustand persist writes to localStorage
    const stored = localStorage.getItem("forge-preferences");
    expect(stored).toBeTruthy();

    const parsed = JSON.parse(stored!);
    expect(parsed.state.cardSize).toBe("small");
  });

  it("hydrates from pre-populated localStorage", () => {
    // Pre-populate localStorage before store reads
    const stored = {
      state: {
        cardSize: "large",
        hudLayout: "floating",
        logDefaultState: "open",
        boardBackground: "green",
      },
      version: 0,
    };
    localStorage.setItem("forge-preferences", JSON.stringify(stored));

    // Force rehydration
    act(() => {
      usePreferencesStore.persist.rehydrate();
    });

    const state = usePreferencesStore.getState();
    expect(state.cardSize).toBe("large");
    expect(state.hudLayout).toBe("floating");
    expect(state.logDefaultState).toBe("open");
    expect(state.boardBackground).toBe("green");
  });
});
