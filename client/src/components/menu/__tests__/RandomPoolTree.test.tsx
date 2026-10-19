import "../../../test/helpers/persistedStorage";

import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { RandomPoolTree } from "../RandomPoolTree";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { DECK_FOLDERS_KEY, DECK_METADATA_KEY } from "../../../constants/storage";
import {
  emptyPoolSelection,
  folderPoolGroup,
  type PoolCandidate,
} from "../../../services/randomPoolSelection";

const ITEMS: PoolCandidate[] = [
  { id: "saved:Burn", name: "Burn", source: { type: "saved" }, savedName: "Burn" },
  { id: "saved:Prowess", name: "Prowess", source: { type: "saved" }, savedName: "Prowess" },
  { id: "saved:Draft", name: "Draft", source: { type: "saved" }, savedName: "Draft" },
  { id: "feed:meta:Tron", name: "Tron", source: { type: "feed", feedId: "meta" } },
  {
    id: "precon:xyz",
    name: "Precon Deck (TST)",
    source: { type: "precon", deckId: "xyz", code: "TST" },
  },
];

function seedLibrary(): void {
  localStorage.setItem(
    DECK_FOLDERS_KEY,
    JSON.stringify([{ id: "f1", name: "Aggro", order: 0 }]),
  );
  localStorage.setItem(
    DECK_METADATA_KEY,
    JSON.stringify({
      Burn: { addedAt: 1, folderId: "f1" },
      Prowess: { addedAt: 2, folderId: "f1" },
    }),
  );
}

beforeEach(() => {
  seedLibrary();
  act(() => {
    usePreferencesStore.setState({
      humanPoolSelection: emptyPoolSelection(),
      aiPoolSelection: emptyPoolSelection(),
      aiBracketFilter: [],
    });
  });
});

afterEach(() => {
  cleanup();
  localStorage.clear();
});

describe("RandomPoolTree", () => {
  it("shows the selection count on the collapsed header", () => {
    render(<RandomPoolTree pool="ai" items={ITEMS} />);
    expect(screen.getByRole("button", { name: /AI random pool 5 of 5/ })).toBeInTheDocument();
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
  });

  it("toggles a whole folder, then lets single decks flip back", async () => {
    const user = userEvent.setup();
    render(<RandomPoolTree pool="ai" items={ITEMS} />);

    await user.click(screen.getByRole("button", { name: /AI random pool/ }));
    await user.click(screen.getByRole("checkbox", { name: "Aggro" }));
    expect(screen.getByRole("button", { name: /AI random pool 3 of 5/ })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Aggro 0 of 2/ }));
    const burn = screen.getByRole("checkbox", { name: "Burn" });
    const prowess = screen.getByRole("checkbox", { name: "Prowess" });
    expect(burn).not.toBeChecked();
    expect(prowess).not.toBeChecked();

    await user.click(burn);
    expect(burn).toBeChecked();
    expect(prowess).not.toBeChecked();
    expect(screen.getByRole("button", { name: /AI random pool 4 of 5/ })).toBeInTheDocument();
    expect(usePreferencesStore.getState().aiPoolSelection).toEqual({
      excludedGroups: [folderPoolGroup("f1")],
      excludedDecks: [],
      includedDecks: ["saved:Burn"],
    });
    // The human pool is untouched: the two pools are independent.
    expect(usePreferencesStore.getState().humanPoolSelection).toEqual(emptyPoolSelection());
  });

  it("warns and offers a reset when everything is excluded", async () => {
    const user = userEvent.setup();
    render(<RandomPoolTree pool="human" items={ITEMS} />);
    await user.click(screen.getByRole("button", { name: /Your random pool/ }));
    for (const name of ["Aggro", "Unfiled", "meta", "Precons"]) {
      await user.click(screen.getByRole("checkbox", { name }));
    }
    expect(screen.getByText(/Everything is excluded/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Reset" }));
    expect(screen.getByRole("button", { name: /Your random pool 5 of 5/ })).toBeInTheDocument();
    expect(screen.queryByText(/Everything is excluded/)).not.toBeInTheDocument();
  });

  it("renders nothing without candidates", () => {
    const { container } = render(<RandomPoolTree pool="ai" items={[]} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("reports playable decks against the draw base", () => {
    // The base holds only Draft, Tron, and the precon: all 5 selected, 3 playable.
    const baseIds = new Set(["saved:Draft", "feed:meta:Tron", "precon:xyz"]);
    render(<RandomPoolTree pool="ai" items={ITEMS} baseIds={baseIds} />);
    expect(
      screen.getByRole("button", { name: /AI random pool 5 of 5.*3 playable/ }),
    ).toBeInTheDocument();
    expect(screen.queryByText(/None of the 5 selected/)).not.toBeInTheDocument();
  });

  it("warns when the selection misses the draw base entirely", async () => {
    const user = userEvent.setup();
    const baseIds = new Set(["saved:Draft", "feed:meta:Tron", "precon:xyz"]);
    render(<RandomPoolTree pool="ai" items={ITEMS} baseIds={baseIds} />);

    // Select only Burn: everything else off, then rescue one deck.
    await user.click(screen.getByRole("button", { name: /AI random pool/ }));
    for (const name of ["Aggro", "Unfiled", "meta", "Precons"]) {
      await user.click(screen.getByRole("checkbox", { name }));
    }
    await user.click(screen.getByRole("button", { name: /Aggro 0 of 2/ }));
    await user.click(screen.getByRole("checkbox", { name: "Burn" }));

    // 1 selected, 0 playable: draws would silently fall back to the 3 base decks.
    expect(
      screen.getByRole("button", { name: /AI random pool 1 of 5.*0 playable/ }),
    ).toBeInTheDocument();
    expect(screen.getByText(/None of the 1 selected decks/)).toBeInTheDocument();
  });

  it("edits the shared AI bracket filter when chips are shown", async () => {
    const user = userEvent.setup();
    render(<RandomPoolTree pool="human" items={ITEMS} showBracketFilter />);
    await user.click(screen.getByRole("button", { name: /Your random pool/ }));
    const chip = screen.getByRole("button", { name: /2 Core/ });
    expect(chip).toHaveAttribute("aria-pressed", "false");
    await user.click(chip);
    expect(chip).toHaveAttribute("aria-pressed", "true");
    expect(usePreferencesStore.getState().aiBracketFilter).toEqual([2]);
  });

  it("hides the bracket chips by default", async () => {
    const user = userEvent.setup();
    render(<RandomPoolTree pool="ai" items={ITEMS} />);
    await user.click(screen.getByRole("button", { name: /AI random pool/ }));
    expect(screen.queryByRole("button", { name: /2 Core/ })).not.toBeInTheDocument();
  });
});
