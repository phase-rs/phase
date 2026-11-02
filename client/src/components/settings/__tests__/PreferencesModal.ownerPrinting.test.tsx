import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import germanCommon from "../../../i18n/locales/de/common.json";
import germanSettings from "../../../i18n/locales/de/settings.json";
import englishCommon from "../../../i18n/locales/en/common.json";
import englishSettings from "../../../i18n/locales/en/settings.json";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { PreferencesModal } from "../PreferencesModal";

vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

describe("PreferencesModal owner printing preference", () => {
  beforeEach(() => {
    act(() => usePreferencesStore.setState(usePreferencesStore.getInitialState()));
    localStorage.removeItem("phase-preferences");
  });

  afterEach(() => {
    cleanup();
    act(() => usePreferencesStore.setState(usePreferencesStore.getInitialState()));
    localStorage.removeItem("phase-preferences");
    vi.clearAllMocks();
  });

  it("toggles only the viewer choice in Visual and reopens with a hydrated opt-out", async () => {
    const siblings = {
      showCardPreviewFooter: false,
      artChain: [{ type: "newest" as const }],
      artOverrides: {
        "oracle-id": {
          scryfallId: "printing-id",
          setCode: "tst",
          collectorNumber: "1",
        },
      },
    };
    act(() => usePreferencesStore.setState(siblings));
    expect(usePreferencesStore.getState()).toMatchObject(siblings);
    const modal = render(<PreferencesModal onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Visual" }));
    const checkbox = screen.getByRole("checkbox", {
      name: englishSettings.visual.showOwnerPrintings,
    });
    expect(checkbox).toBeChecked();

    for (const show of [false, true]) {
      fireEvent.click(checkbox);
      expect(checkbox).toHaveProperty("checked", show);
      expect(usePreferencesStore.getState().showOwnerPrintings).toBe(show);
      expect(JSON.parse(localStorage.getItem("phase-preferences")!).state.showOwnerPrintings).toBe(show);
      expect(usePreferencesStore.getState()).toMatchObject(siblings);
    }

    fireEvent.click(checkbox);
    const savedOff = localStorage.getItem("phase-preferences")!;
    modal.unmount();
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(true));
    expect(usePreferencesStore.getState().showOwnerPrintings).toBe(true);
    localStorage.setItem("phase-preferences", savedOff);
    await act(async () => { await usePreferencesStore.persist.rehydrate(); });
    render(<PreferencesModal onClose={vi.fn()} initialTab="visual" />);
    expect(screen.getByRole("checkbox", {
      name: englishSettings.visual.showOwnerPrintings,
    })).not.toBeChecked();
    expect(usePreferencesStore.getState().showOwnerPrintings).toBe(false);
    expect(usePreferencesStore.getState()).toMatchObject(siblings);
  });

  it("resolves the owner title, checkbox and hint from the real German catalog", async () => {
    const localized = createInstance();
    await localized.init({
      lng: "de",
      fallbackLng: "en",
      defaultNS: "common",
      resources: {
        de: { common: germanCommon, settings: germanSettings },
        en: { common: englishCommon, settings: englishSettings },
      },
      interpolation: { escapeValue: false },
      react: { useSuspense: false },
    });
    render(
      <I18nextProvider i18n={localized}>
        <PreferencesModal onClose={vi.fn()} initialTab="visual" />
      </I18nextProvider>,
    );
    expect(screen.getByText(germanSettings.visual.ownerCardArt)).toBeInTheDocument();
    expect(screen.getByText(germanSettings.visual.ownerPrintingsHint)).toBeInTheDocument();
    const checkbox = screen.getByRole("checkbox", {
      name: germanSettings.visual.showOwnerPrintings,
    });
    expect(checkbox).toBeChecked();
    fireEvent.click(checkbox);
    expect(checkbox).not.toBeChecked();
    expect(usePreferencesStore.getState().showOwnerPrintings).toBe(false);
  });
});
