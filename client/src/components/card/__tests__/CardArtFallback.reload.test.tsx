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

  it("keeps the button outside the image role, so assistive tech can reach it", () => {
    render(
      <CardArtFallback name={CARD_NAME} oracleText={RULES_TEXT} variant="preview" className="h-96 w-72" onReload={vi.fn()} />,
    );

    const tile = screen.getByRole("img", { name: CARD_NAME });
    expect(tile).toHaveTextContent("less to cast.");
    expect(tile).not.toContainElement(screen.getByRole("button", { name: RELOAD }));
  });

  it("draws no button without a reload", () => {
    render(<CardArtFallback name={CARD_NAME} variant="preview" className="h-96 w-72" />);

    expect(screen.getByRole("img", { name: CARD_NAME })).toHaveTextContent(CARD_NAME);
    expect(screen.queryByRole("button")).toBeNull();
  });

  // happy-dom applies no CSS, so the text size is pinned by class.
  it("sets the preview tile's name and rules text larger than a board tile's", () => {
    const { rerender } = render(<CardArtFallback name={CARD_NAME} oracleText={RULES_TEXT} className="h-40 w-28" />);
    // The tile's text is its name, then its rules text.
    const boardText = screen.getByRole("img", { name: CARD_NAME });
    expect(boardText.firstElementChild).toHaveClass("text-xs");
    expect(boardText.lastElementChild).toHaveClass("text-[10px]");
    expect(boardText.lastElementChild).toHaveTextContent("less to cast.");

    rerender(<CardArtFallback name={CARD_NAME} oracleText={RULES_TEXT} variant="preview" className="h-96 w-72" />);
    const previewText = screen.getByRole("img", { name: CARD_NAME });
    expect(previewText.firstElementChild).toHaveClass("text-base");
    expect(previewText.lastElementChild).toHaveClass("text-sm");
    expect(previewText.lastElementChild).toHaveTextContent("less to cast.");
  });
});
