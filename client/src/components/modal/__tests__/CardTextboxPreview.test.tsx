import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useCardImage } from "../../../hooks/useCardImage.ts";
import { CardTextboxPreview } from "../CardTextboxPreview.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({ src: null, isLoading: true })),
}));

const mockUseCardImage = vi.mocked(useCardImage);

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("CardTextboxPreview art fallback (issue #6156)", () => {
  it("stays absent while the lookup is in flight", () => {
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: true,
      isRotated: false,
      isFlip: false,
    });

    const { container } = render(<CardTextboxPreview cardName="Banana" />);

    // Absent rather than a flashed band — the modal shouldn't jitter.
    expect(container).toBeEmptyDOMElement();
  });

  it("names the card when the lookup settles with no art", () => {
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(<CardTextboxPreview cardName="Banana" />);

    // Previously this returned null, erasing the identification band from the
    // choice modals for exactly the cards hardest to identify.
    expect(screen.getByRole("img", { name: "Banana" })).toBeInTheDocument();
    expect(screen.getByText("Banana")).toBeInTheDocument();
  });

  it("falls back to the named strip when the resolved art fails to load", () => {
    // Requested by maintainer review: the artError branch existed but no
    // onError handler could ever set it, so the branch was unreachable.
    mockUseCardImage.mockReturnValue({
      src: "https://example.invalid/banana.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(<CardTextboxPreview cardName="Banana" />);

    const img = document.querySelector("img");
    expect(img).not.toBeNull();

    fireEvent.error(img!);

    expect(screen.getByRole("img", { name: "Banana" })).toBeInTheDocument();
    expect(document.querySelector("img")).toBeNull();
  });
});
