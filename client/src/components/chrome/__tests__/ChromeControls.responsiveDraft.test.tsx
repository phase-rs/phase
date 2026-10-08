import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ChromeControls } from "../ChromeControls";

vi.mock("../../../stores/preferencesStore", () => ({
  usePreferencesStore: (selector: (state: { language: string }) => unknown) => selector({ language: "en" }),
}));
vi.mock("../../settings/PreferencesModal", () => ({ PreferencesModal: () => null }));
vi.mock("../../ui/LanguageFlag", () => ({ LanguageFlag: () => <span data-testid="language-flag" /> }));
vi.mock("../AccountControl", () => ({ AccountControl: () => <div data-testid="account-control" /> }));
vi.mock("../FullscreenButton", () => ({ FullscreenButton: () => <div data-testid="fullscreen-control" /> }));
vi.mock("../VolumeControl", () => ({ VolumeControl: () => <div data-testid="volume-control" /> }));

describe("ChromeControls responsive draft visibility", () => {
  afterEach(cleanup);

  it("hides Volume and Language but retains Account and Settings for phone draft chrome", () => {
    render(<ChromeControls hideVolume hideLanguage />);

    expect(screen.queryByTestId("volume-control")).not.toBeInTheDocument();
    expect(screen.queryByTestId("language-flag")).not.toBeInTheDocument();
    expect(screen.getByTestId("account-control")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Settings" })).toBeInTheDocument();
    expect(screen.getByTestId("fullscreen-control")).toBeInTheDocument();
  });

  it("shows Volume and Language by default", () => {
    render(<ChromeControls />);

    expect(screen.getByTestId("volume-control")).toBeInTheDocument();
    expect(screen.getByTestId("language-flag")).toBeInTheDocument();
  });
});