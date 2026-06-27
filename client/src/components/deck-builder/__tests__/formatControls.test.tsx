import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CardSearch, type CardSearchFilters } from "../CardSearch";
import { FormatFilter } from "../FormatFilter";

vi.mock("../../../hooks/useSetList", () => ({
  useSetList: () => null,
}));

vi.mock("../../../services/engineRuntime", () => ({
  searchCards: vi.fn(),
}));

const EMPTY_FILTERS: CardSearchFilters = {
  text: "",
  colors: [],
  type: "",
  cmcMax: undefined,
  sets: [],
  browseFormat: "all",
};

afterEach(cleanup);

describe("deck-builder format controls", () => {
  it("does not show Two-Headed Giant in the deck format filter", () => {
    render(<FormatFilter selected="Standard" onChange={vi.fn()} />);

    expect(screen.getByRole("button", { name: "Standard" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Two-Headed Giant" })).not.toBeInTheDocument();
  });

  it("does not show Two-Headed Giant in card-search legality filters", async () => {
    const user = userEvent.setup();
    render(
      <CardSearch
        onResults={vi.fn()}
        filters={EMPTY_FILTERS}
        onFiltersChange={vi.fn()}
        onReset={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Legal in format" }));

    expect(screen.getByRole("option", { name: "Standard" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: "Two-Headed Giant" })).not.toBeInTheDocument();
  });
});
