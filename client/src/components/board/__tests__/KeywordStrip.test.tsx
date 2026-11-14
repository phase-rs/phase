import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { Keyword } from "../../../adapter/types";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { KeywordStrip } from "../KeywordStrip";

describe("KeywordStrip", () => {
  afterEach(async () => {
    await act(async () => {
      usePreferencesStore.getState().setLanguage("en");
    });
  });

  it("re-renders translated keyword details on a language change with unchanged props", async () => {
    const keywords: Keyword[] = [
      { Splice: { subtype: "Arcane", cost: { type: "Cost", shards: ["Blue"], generic: 1 } } },
      { Foretell: { type: "SelfManaCost" } },
    ];
    const baseKeywords: Keyword[] = [];
    const props = { keywords, baseKeywords, badgeSize: "24px", maxVisible: 4 };

    const { rerender } = render(<KeywordStrip {...props} />);
    expect(screen.getByText("onto Arcane {1}{U}")).toBeInTheDocument();
    expect(screen.getByText("its mana cost")).toBeInTheDocument();

    await act(async () => {
      usePreferencesStore.getState().setLanguage("de");
    });
    rerender(<KeywordStrip {...props} />);

    expect(screen.getByText("auf Arcane {1}{U}")).toBeInTheDocument();
    expect(screen.getByText("seine Manakosten")).toBeInTheDocument();
    expect(screen.queryByText("onto Arcane {1}{U}")).not.toBeInTheDocument();
  });
});
