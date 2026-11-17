import type { ComponentProps } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ChoiceModal } from "../ChoiceModal.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({
    src: null,
    isLoading: false,
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
const DEFAULT_SCREEN = { innerWidth: window.innerWidth, innerHeight: window.innerHeight };

function renderChoice(props: Partial<ComponentProps<typeof ChoiceModal>> = {}) {
  render(
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
    />,
  );
}

afterEach(() => {
  cleanup();
  Object.assign(window, DEFAULT_SCREEN);
});

describe("ChoiceModal layout", () => {
  it("keeps a prompt that does not opt in stacked, even on a phone on its side", () => {
    // Assignment, not `Object.defineProperty`: vitest passes a set on to
    // happy-dom, whose `matchMedia` reads its own size.
    Object.assign(window, { innerWidth: 844, innerHeight: 390 });
    // Reach guard: this is the screen the optional-effect prompt splits on.
    expect(window.matchMedia("(max-height: 500px) and (orientation: landscape)").matches).toBe(true);

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
  });
});
