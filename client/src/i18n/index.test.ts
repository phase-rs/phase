import { afterEach, describe, expect, it, vi } from "vitest";

describe("i18n production bootstrap", () => {
  afterEach(() => {
    localStorage.clear();
    document.documentElement.lang = "en";
    vi.resetModules();
  });

  it("keeps the document language synchronized with the preferences store", async () => {
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({ state: { language: "fr" }, version: 31 }),
    );
    document.documentElement.lang = "en";
    vi.resetModules();

    const { default: i18n } = await import("./index");
    const { usePreferencesStore } = await import("../stores/preferencesStore");

    expect(usePreferencesStore.getState().language).toBe("fr");
    expect(i18n.language).toBe("fr");
    expect(document.documentElement.lang).toBe("fr");

    usePreferencesStore.getState().setLanguage("de");

    expect(document.documentElement.lang).toBe("de");
    await vi.waitFor(() => expect(i18n.language).toBe("de"));
  });
});
