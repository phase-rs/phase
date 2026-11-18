import type { ComponentProps } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SHORT_LANDSCAPE_QUERY } from "../../../constants/ui.ts";
import { ChoiceModal } from "../ChoiceModal.tsx";

const cardImage = vi.hoisted(() => ({ isLoading: false, src: null as string | null }));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({
    src: cardImage.src,
    isLoading: cardImage.isLoading,
    isRotated: false,
    isFlip: false,
  })),
}));

/**
 * Issue #11006: `layout="columns"` is the opt-in that splits a prompt into the
 * card beside its actions on a phone on its side. Only the optional-effect
 * prompt opts in; every other `ChoiceModal` keeps the stacked layout on every
 * screen. happy-dom does no layout, so these pin the structure.
 */

const CARD_NAME = "Rhystic Study";
const HOLD_HINT = "Hold to see the full card";
const CARD_ART = "https://cards.example/rhystic-study.jpg";
const DEFAULT_SCREEN = { innerWidth: window.innerWidth, innerHeight: window.innerHeight };

function choiceModal(props: Partial<ComponentProps<typeof ChoiceModal>> = {}) {
  return (
    <ChoiceModal
      title="Rhystic Study - Optional Effect"
      options={[
        { id: "accept", label: "Yes" },
        { id: "decline", label: "No" },
      ]}
      onChoose={vi.fn()}
      previewCardName={CARD_NAME}
      previewObjectId={100}
      footer={
        <label>
          <input type="checkbox" />
          Remember
        </label>
      }
      {...props}
    />
  );
}

function renderChoice(props: Partial<ComponentProps<typeof ChoiceModal>> = {}) {
  return render(choiceModal(props));
}

afterEach(() => {
  cleanup();
  Object.assign(window, DEFAULT_SCREEN);
  cardImage.isLoading = false;
  cardImage.src = null;
});

describe("ChoiceModal layout", () => {
  it("keeps a prompt that does not opt in stacked, even on a phone on its side", () => {
    // Assignment, not `Object.defineProperty`: vitest passes a set on to
    // happy-dom, whose `matchMedia` reads its own size.
    Object.assign(window, { innerWidth: 844, innerHeight: 390 });
    // Reach guard: this is the screen the optional-effect prompt splits on.
    expect(window.matchMedia(SHORT_LANDSCAPE_QUERY).matches).toBe(true);

    renderChoice();

    const previewBlock = screen.getByRole("img", { name: CARD_NAME }).parentElement as HTMLElement;
    const options = screen.getByRole("button", { name: "Yes" }).parentElement as HTMLElement;
    const optionsBlock = options.parentElement as HTMLElement;
    const footerBlock = screen.getByLabelText("Remember").closest("label")?.parentElement;

    expect(screen.getByRole("dialog")).toHaveClass("max-w-sm");
    expect(previewBlock.nextElementSibling).toBe(optionsBlock);
    expect(screen.getByRole("button", { name: "No" }).parentElement).toBe(options);
    expect(options).toHaveClass("flex", "flex-col");
    expect(Array.from(optionsBlock.children)).toEqual([options, footerBlock]);
    expect(screen.queryByText(HOLD_HINT)).not.toBeInTheDocument();
  });

  it("puts the card in a left column, and the options side by side above the footer in a right column", () => {
    renderChoice({ layout: "columns" });

    const actionRow = screen.getByRole("button", { name: "Yes" }).parentElement as HTMLElement;
    const actionColumn = actionRow.parentElement as HTMLElement;
    const columns = actionColumn.parentElement as HTMLElement;
    const cardColumn = columns.firstElementChild as HTMLElement;

    expect(screen.getByRole("dialog")).toHaveClass("max-w-3xl");
    expect(columns).toHaveClass("grid", "grid-cols-2");
    expect(Array.from(columns.children)).toEqual([cardColumn, actionColumn]);
    expect(cardColumn).toContainElement(screen.getByRole("img", { name: CARD_NAME }));
    expect(cardColumn).toContainElement(screen.getByText(HOLD_HINT));
    expect(screen.getByRole("button", { name: "No" }).parentElement).toBe(actionRow);
    expect(actionRow).toHaveClass("grid", "grid-cols-2");
    expect(actionColumn).toContainElement(screen.getByLabelText("Remember"));
  });

  it("keeps the columns while the card's art loads, so the prompt does not jump when it arrives", () => {
    cardImage.isLoading = true;

    const { rerender } = renderChoice({ layout: "columns" });

    const actionRow = screen.getByRole("button", { name: "Yes" }).parentElement as HTMLElement;
    const actionColumn = actionRow.parentElement as HTMLElement;
    const columns = actionColumn.parentElement as HTMLElement;
    const cardColumn = columns.firstElementChild as HTMLElement;

    // Reach guard: the art is still loading, so the card column has no card yet.
    expect(screen.queryByRole("img", { name: CARD_NAME })).not.toBeInTheDocument();
    expect(screen.getByRole("dialog")).toHaveClass("max-w-3xl");
    expect(columns).toHaveClass("grid", "grid-cols-2");
    // The card column holds its place, with the hint, so Yes and No already
    // sit in the right column and stay there.
    expect(columns.children).toHaveLength(2);
    expect(columns.lastElementChild).toBe(actionColumn);
    expect(cardColumn).toContainElement(screen.getByText(HOLD_HINT));
    expect(actionRow).toHaveClass("grid", "grid-cols-2");

    cardImage.isLoading = false;
    cardImage.src = CARD_ART;
    rerender(choiceModal({ layout: "columns" }));

    // The art band lands at the top of the same card column, still in columns,
    // and nothing else moves.
    const art = document.querySelector(`img[src="${CARD_ART}"]`);
    expect(art).toBeInTheDocument();
    expect(screen.getByRole("dialog")).toHaveClass("max-w-3xl");
    expect(columns).toHaveClass("grid", "grid-cols-2");
    expect(cardColumn.firstElementChild).toBe(art?.parentElement);
    expect(screen.queryByRole("img", { name: CARD_NAME })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Yes" }).parentElement).toBe(actionRow);
  });

  it("offers the hold hint only when holding the card opens its preview", () => {
    renderChoice({ layout: "columns", previewObjectId: undefined });

    // Reach guard: the columns layout rendered, card column included.
    expect(screen.getByRole("dialog")).toHaveClass("max-w-3xl");
    expect(screen.getByRole("img", { name: CARD_NAME })).toBeInTheDocument();
    expect(screen.queryByText(HOLD_HINT)).not.toBeInTheDocument();
  });

  it("stacks a columns prompt that has no card for the left column", () => {
    renderChoice({ layout: "columns", previewCardName: undefined });

    const options = screen.getByRole("button", { name: "Yes" }).parentElement as HTMLElement;

    expect(screen.getByRole("dialog")).toHaveClass("max-w-sm");
    expect(options).toHaveClass("flex", "flex-col");
    expect(options).not.toHaveClass("grid-cols-2");
    expect(screen.queryByText(HOLD_HINT)).not.toBeInTheDocument();

    // Paired positive: the same prompt with a card does split.
    cleanup();
    renderChoice({ layout: "columns" });
    expect(screen.getByRole("dialog")).toHaveClass("max-w-3xl");
  });
});
