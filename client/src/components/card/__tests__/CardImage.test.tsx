import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useCardImage } from "../../../hooks/useCardImage.ts";
import { CardImage } from "../CardImage.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({
    src: null,
    isLoading: true,
    isRotated: false,
    isFlip: false,
  })),
}));

// The engine card DB has no entry for a token like `Banana` (issue #6156), so
// the component's own Oracle-text lookup returns nothing; tests pass Oracle text
// explicitly via the prop when they want to exercise that branch.
vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
}));

const mockUseCardImage = vi.mocked(useCardImage);

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("CardImage art fallback (issue #6156)", () => {
  it("shows the loading pulse (not the text tile) while art is resolving", () => {
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: true,
      isRotated: false,
      isFlip: false,
    });

    render(<CardImage cardName="Banana" isToken />);

    // The deliberate text tile carries role="img"; the loading pulse does not.
    expect(screen.queryByRole("img")).toBeNull();
    // No visible name text while loading — the pulse is featureless by design.
    expect(screen.queryByText("Banana")).toBeNull();
  });

  it("renders the name text tile for an artless token once resolution finishes with no src", () => {
    // Kibo, Uktabi Prince's Banana: no official paper printing → null token src.
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(<CardImage cardName="Banana" isToken />);

    const tile = screen.getByRole("img", { name: "Banana" });
    expect(tile).toBeInTheDocument();
    expect(screen.getByText("Banana")).toBeInTheDocument();
    // No <img> element is emitted for the artless case, so nothing can render as
    // a broken/black square.
    expect(document.querySelector("img")).toBeNull();
  });

  it("includes the Oracle text in the fallback tile when it is known", () => {
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(
      <CardImage
        cardName="Banana"
        isToken
        oracleText="{T}, Sacrifice this artifact: Add one mana of any color. You gain 1 life."
      />,
    );

    const tile = screen.getByRole("img", { name: "Banana" });
    // Use textContent so the assertion is robust to how RichLabel segments the
    // text around mana symbols (the "{T}" renders as a symbol, not text).
    expect(tile.textContent).toContain("You gain 1 life.");
  });

  it("falls back to the name text tile when a resolved image fails to load", () => {
    mockUseCardImage.mockReturnValue({
      src: "https://example.invalid/banana.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(<CardImage cardName="Reveillark" />);

    // The <img> renders first...
    const img = document.querySelector("img");
    expect(img).not.toBeNull();

    // ...then a load failure swaps in the same text tile.
    fireEvent.error(img!);

    expect(screen.getByRole("img", { name: "Reveillark" })).toBeInTheDocument();
    expect(screen.getByText("Reveillark")).toBeInTheDocument();
  });
});
