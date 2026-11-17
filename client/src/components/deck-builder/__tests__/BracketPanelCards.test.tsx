import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import type { BracketCard } from "../../../types/bracket";
import { BracketPanelCardList } from "../BracketPanelCards";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const deck = (name: string): BracketCard => ({ name, role: "deck" });

describe("BracketPanelCardList", () => {
  it("keeps the spaces in a comma list", () => {
    const { container } = render(
      <BracketPanelCardList
        cards={[deck("A"), deck("B"), deck("C")]}
        joiner="comma"
        onCardClick={vi.fn()}
      />,
    );
    expect(container.textContent).toBe("A, B, C");
    // A flex container drops the spaces at the ends of each separator, so the
    // browser showed "A,B,C". The test DOM does no layout, so pin the container.
    const list = container.firstElementChild;
    expect(list).not.toHaveClass("flex");
    expect(list).not.toHaveClass("inline-flex");
  });

  it("keeps the spaces in an or list", () => {
    const { container } = render(
      <BracketPanelCardList cards={[deck("A"), deck("B")]} joiner="or" onCardClick={vi.fn()} />,
    );
    expect(container.textContent).toBe("A or B");
  });

  it("renders a card listed twice without a duplicate-key warning", () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <BracketPanelCardList
        cards={[deck("Island"), deck("Island")]}
        joiner="comma"
        onCardClick={vi.fn()}
      />,
    );
    expect(screen.getAllByRole("button", { name: "Island" })).toHaveLength(2);
    expect(error).not.toHaveBeenCalled();
  });
});
