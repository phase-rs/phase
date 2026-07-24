import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { RichLabel } from "../RichLabel.tsx";

describe("RichLabel", () => {
  it("renders valid mana notation as a symbol", () => {
    render(<RichLabel text="Pay {G}." />);

    expect(screen.getByAltText("G")).toBeInTheDocument();
  });

  it("keeps non-mana brace content as text", () => {
    render(<RichLabel text="Pay Fixed { value: 2 } life" />);

    expect(screen.getByText("{ value: 2 }")).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });
});
