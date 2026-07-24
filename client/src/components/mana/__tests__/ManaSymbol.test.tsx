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

    expect(screen.getByText("Pay Fixed { value: 2 } life")).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });

  it.each(["2/W", "W/U/P"])("renders supported composite notation %s as a symbol", (shard) => {
    render(<RichLabel text={`Pay {${shard}}.`} />);

    expect(screen.getByAltText(shard)).toBeInTheDocument();
  });

  it.each(["W/X", "2/X"])("keeps unsupported composite notation %s as text", (shard) => {
    render(<RichLabel text={`Pay {${shard}}.`} />);

    expect(screen.getByText(`Pay {${shard}}.`)).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });
});
