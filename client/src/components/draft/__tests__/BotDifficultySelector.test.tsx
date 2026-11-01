import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore";
import { BotDifficultySelector } from "../BotDifficultySelector";

const CHECKBOX = "Use built-in AI with draft pick enhancements";

describe("BotDifficultySelector pick enhancements", () => {
  beforeEach(() => {
    usePreferencesStore.getState().setDraftPickEnhancements(false);
  });
  afterEach(cleanup);

  it("omits the checkbox unless the surface opts in", () => {
    render(<BotDifficultySelector />);
    expect(screen.queryByRole("checkbox")).toBeNull();
  });

  it("writes the checkbox to the persisted preference", () => {
    render(<BotDifficultySelector showPickEnhancements />);
    const checkbox = screen.getByRole("checkbox", { name: new RegExp(CHECKBOX) });
    expect(checkbox).not.toBeChecked();

    fireEvent.click(checkbox);
    expect(usePreferencesStore.getState().draftPickEnhancements).toBe(true);
    expect(checkbox).toBeChecked();
  });
});
