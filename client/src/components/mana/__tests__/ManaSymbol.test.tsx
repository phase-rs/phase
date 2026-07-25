import { render, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { RichLabel } from "../RichLabel.tsx";

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
});
