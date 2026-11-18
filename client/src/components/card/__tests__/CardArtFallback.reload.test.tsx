import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CardArtFallback } from "../CardArtFallback.tsx";

// Regression tests for #11008: the mobile preview's artless tile offers a
// one-tap reload, the tap never reaches the preview around it, and its text
// reads at the preview's size.

const CARD_NAME = "Etherium Sculptor";
const RULES_TEXT = "Artifact spells you cast cost {1} less to cast.";
const RELOAD = "Tap to reload art";

afterEach(() => {
  cleanup();
});

describe("CardArtFallback reload", () => {
  it("keeps the tap on its 44px button and off the surface around the tile", () => {
    const onReload = vi.fn();
    const onSurfacePointerDown = vi.fn();
    const onSurfaceClick = vi.fn();
    render(
      <div onPointerDown={onSurfacePointerDown} onClick={onSurfaceClick}>
        <CardArtFallback
          name={CARD_NAME}
          oracleText={RULES_TEXT}
          variant="preview"
          className="h-96 w-72"
          onReload={onReload}
        />
      </div>,
    );

    const reload = screen.getByRole("button", { name: RELOAD });
    expect(reload).toHaveClass("min-h-11");
    fireEvent.pointerDown(reload);
    fireEvent.click(reload);
    expect(onReload).toHaveBeenCalledTimes(1);
    expect(onSurfacePointerDown).not.toHaveBeenCalled();
    expect(onSurfaceClick).not.toHaveBeenCalled();

    // Reach guard: a tap on the tile itself does reach the surface, so the
    // assertions above are not passing because nothing listens.
    const tile = screen.getByRole("img", { name: CARD_NAME });
    fireEvent.pointerDown(tile);
    fireEvent.click(tile);
    expect(onSurfacePointerDown).toHaveBeenCalledTimes(1);
    expect(onSurfaceClick).toHaveBeenCalledTimes(1);
  });

  // An `img` role makes everything inside it presentational, so a screen
  // reader would read neither the rules text nor the button inside it.
  it("keeps the rules text and the button outside the image role, so assistive tech reads and reaches them", () => {
    render(
      <CardArtFallback name={CARD_NAME} oracleText={RULES_TEXT} variant="preview" className="h-96 w-72" onReload={vi.fn()} />,
    );

    // Reach guard: the tile names itself by its image role.
    expect(screen.getByRole("img", { name: CARD_NAME })).toBeInTheDocument();
    const rules = screen.getByText(/less to cast\./);
    expect(rules.closest("[role='img']")).toBeNull();
    expect(screen.getByRole("button", { name: RELOAD }).closest("[role='img']")).toBeNull();
  });

  it("draws no button without a reload", () => {
    render(<CardArtFallback name={CARD_NAME} variant="preview" className="h-96 w-72" />);

    expect(screen.getByRole("img", { name: CARD_NAME })).toHaveTextContent(CARD_NAME);
    expect(screen.queryByRole("button")).toBeNull();
  });

  // happy-dom applies no CSS, so the text size is pinned by class.
  it("sets the preview tile's name and rules text larger than a board tile's", () => {
    const { rerender } = render(<CardArtFallback name={CARD_NAME} oracleText={RULES_TEXT} className="h-40 w-28" />);
    // The tile's name carries its image role; its rules text follows it.
    const boardName = screen.getByRole("img", { name: CARD_NAME });
    expect(boardName).toHaveClass("text-xs");
    expect(boardName.nextElementSibling).toHaveClass("text-[10px]");
    expect(boardName.nextElementSibling).toHaveTextContent("less to cast.");

    rerender(<CardArtFallback name={CARD_NAME} oracleText={RULES_TEXT} variant="preview" className="h-96 w-72" />);
    const previewName = screen.getByRole("img", { name: CARD_NAME });
    expect(previewName).toHaveClass("text-base");
    expect(previewName.nextElementSibling).toHaveClass("text-sm");
    expect(previewName.nextElementSibling).toHaveTextContent("less to cast.");
  });
});
