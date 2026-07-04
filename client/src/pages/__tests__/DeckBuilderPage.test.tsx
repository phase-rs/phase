import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import type { GameFormat } from "../../adapter/types";
import type { CardSearchFilters } from "../../components/deck-builder/CardSearch";
import { DeckBuilderPage } from "../DeckBuilderPage";

let capturedProps:
  | {
      format: GameFormat;
      searchFilters: CardSearchFilters;
    }
  | undefined;

vi.mock("../../audio/useAudioContext", () => ({
  useAudioContext: () => undefined,
}));

vi.mock("../../hooks/useAltToggle", () => ({
  useAltToggle: () => undefined,
}));

vi.mock("../../components/card/CardPreview", () => ({
  CardPreview: () => null,
}));

vi.mock("../../components/deck-builder/DeckBuilder", () => ({
  DeckBuilder: (props: { format: GameFormat; searchFilters: CardSearchFilters }) => {
    capturedProps = {
      format: props.format,
      searchFilters: props.searchFilters,
    };
    return <div>Deck Builder</div>;
  },
}));

function renderDeckBuilderPage(initialEntry: string) {
  return render(
    <MemoryRouter initialEntries={[initialEntry]}>
      <Routes>
        <Route path="/deck-builder" element={<DeckBuilderPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

beforeEach(() => {
  capturedProps = undefined;
});

afterEach(cleanup);

describe("DeckBuilderPage format routing", () => {
  it("falls back when the URL deck format is Two-Headed Giant", () => {
    renderDeckBuilderPage("/deck-builder?format=twoheadedgiant");

    expect(capturedProps?.format).toBe("Standard");
  });

  it("accepts Planechase as a deck-builder format", () => {
    renderDeckBuilderPage("/deck-builder?format=planechase");

    expect(capturedProps?.format).toBe("Planechase");
  });

  it("falls back when the URL browse legality filter is Two-Headed Giant", () => {
    renderDeckBuilderPage("/deck-builder?browseFormat=twoheadedgiant");

    expect(capturedProps?.searchFilters.browseFormat).toBe("Standard");
  });

  it("accepts Planechase as a browse legality filter", () => {
    renderDeckBuilderPage("/deck-builder?browseFormat=planechase");

    expect(capturedProps?.searchFilters.browseFormat).toBe("Planechase");
  });
});
