import { cleanup, fireEvent, render, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { RichLabel } from "../RichLabel.tsx";

const useManaSymbolImage = vi.hoisted(() => vi.fn());

vi.mock("../../../hooks/useFixedVisualImage.ts", () => ({
  useManaSymbolImage,
}));

beforeEach(() => {
  useManaSymbolImage.mockImplementation((shard: string | null) => ({
    src: shard ? `visual-pack://mana/${encodeURIComponent(shard)}` : null,
    isLoading: false,
    advanceFailedSource: vi.fn(),
  }));
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("RichLabel", () => {
  it("renders valid mana notation as a symbol", () => {
    const { container } = render(<RichLabel text="Pay {G}." />);

    expect(within(container).getByAltText("G")).toBeInTheDocument();
  });

  it("keeps non-mana brace content as text", () => {
    const { container } = render(<RichLabel text="Pay Fixed { value: 2 } life" />);

    expect(within(container).getByText("Pay Fixed { value: 2 } life")).toBeInTheDocument();
    expect(within(container).queryByRole("img")).not.toBeInTheDocument();
  });

  it.each(["2/W", "W/U/P"])("renders supported composite notation %s as a symbol", (shard) => {
    const { container } = render(<RichLabel text={`Pay {${shard}}.`} />);

    expect(within(container).getByAltText(shard)).toBeInTheDocument();
  });

  it.each(["W/X", "2/X"])("keeps unsupported composite notation %s as text", (shard) => {
    const { container } = render(<RichLabel text={`Pay {${shard}}.`} />);

    expect(within(container).getByText(`Pay {${shard}}.`)).toBeInTheDocument();
    expect(within(container).queryByRole("img")).not.toBeInTheDocument();
  });

  it.each(["0", "20", "100", "1000000", "∞", "½", "CHAOS"])(
    "admits finite symbol %s",
    (shard) => {
      const { container } = render(<RichLabel text={`Pay {${shard}}.`} />);

      expect(within(container).getByAltText(shard)).toHaveAttribute(
        "src",
        `visual-pack://mana/${encodeURIComponent(shard)}`,
      );
      expect(useManaSymbolImage).toHaveBeenCalledWith(shard);
    },
  );

  it.each(["21", "37", "999", "W/X", "not-mana"])(
    "keeps unsupported finite-catalog input %s text-only",
    (shard) => {
      const { container } = render(<RichLabel text={`Pay {${shard}}.`} />);

      expect(within(container).getByText(`Pay {${shard}}.`)).toBeInTheDocument();
      expect(within(container).queryByRole("img")).not.toBeInTheDocument();
      expect(useManaSymbolImage).not.toHaveBeenCalled();
    },
  );

  it("forwards the exact rendered source on image failure", () => {
    const advanceFailedSource = vi.fn();
    useManaSymbolImage.mockReturnValue({
      src: "visual-pack://mana/W",
      isLoading: false,
      advanceFailedSource,
    });
    const { container } = render(<RichLabel text="Pay {W}." />);

    fireEvent.error(within(container).getByAltText("W"));
    expect(advanceFailedSource).toHaveBeenCalledWith("visual-pack://mana/W");
  });
});
