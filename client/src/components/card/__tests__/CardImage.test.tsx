import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useCardImage } from "../../../hooks/useCardImage.ts";
import { CARD_BACK_URL } from "../../../services/scryfall.ts";
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

    // Positive guard first: the pulse element must actually be in the document.
    // Without this the two negative assertions below would also pass if the
    // loading branch rendered nothing at all, or threw.
    expect(screen.getByLabelText("Loading Banana")).toBeInTheDocument();
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

  it("never shows the artless tile while a lookup is still in flight", () => {
    // Guards a regression this PR briefly shipped on the mobile preview: the
    // fallback was derived from `!src` alone, but useCardImage assigns `src` in
    // a post-render effect, so `src` is null on EVERY first paint. Deriving the
    // tile from `!src` without consulting `isLoading` flashes "no art" before
    // every normal card's art. Only a SETTLED lookup may show the tile.
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: true,
      isRotated: false,
      isFlip: false,
    });

    render(<CardImage cardName="Lightning Bolt" />);

    expect(screen.queryByRole("img", { name: "Lightning Bolt" })).toBeNull();
    expect(screen.getByLabelText("Loading Lightning Bolt")).toBeInTheDocument();
  });

  it("keeps the unimplemented-mechanics badge visible on the fallback tile", () => {
    // The fallback is swapped in place of the <img> rather than early-returned,
    // so the overlay badges survive. An artless card losing its "!" warning
    // would trade one information loss for another.
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(<CardImage cardName="Banana" isToken unimplementedMechanics={["Food"]} />);

    expect(screen.getByRole("img", { name: "Banana" })).toBeInTheDocument();
    expect(screen.getByText("!")).toBeInTheDocument();
  });

  it("keeps face-down cards on the card back instead of revealing a name tile", () => {
    // Face-down cards call `useCardImage("")`, which resolves to a null src with
    // no in-flight lookup — the same shape as an artless token. Only the
    // `!faceDown` guards keep them on the card-back path, so this pins the one
    // case where `src === null` must NOT produce a name tile: a regression here
    // would leak hidden card names to opponents.
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(<CardImage cardName="Grizzly Bears" faceDown />);

    const img = document.querySelector("img");
    expect(img).not.toBeNull();
    expect(img!.getAttribute("src")).toBe(CARD_BACK_URL);
    expect(screen.queryByRole("img", { name: "Grizzly Bears" })).toBeNull();
    expect(screen.queryByText("Grizzly Bears")).toBeNull();
  });

  it("re-tries the image when the art source changes after a load failure", () => {
    // A single component instance survives a permanent turning face up or a DFC
    // transforming. Without a reset the latched `imageError` would pin the text
    // tile in place even once a loadable face arrives.
    mockUseCardImage.mockReturnValue({
      src: "https://example.invalid/front.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    const { rerender } = render(<CardImage cardName="Delver of Secrets" />);
    fireEvent.error(document.querySelector("img")!);
    expect(screen.getByRole("img", { name: "Delver of Secrets" })).toBeInTheDocument();

    // The transformed face resolves to a different, loadable src.
    mockUseCardImage.mockReturnValue({
      src: "https://example.invalid/back.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    rerender(<CardImage cardName="Insectile Aberration" />);

    const img = document.querySelector("img");
    expect(img).not.toBeNull();
    expect(img!.getAttribute("src")).toBe("https://example.invalid/back.png");
  });
});
