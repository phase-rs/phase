import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ShellProvider } from "../../chrome/ShellContext";
import { MenuShell } from "../MenuShell";

describe("MenuShell", () => {
  it("reduces embedded top padding for shell-owned phone progress", () => {
    const { container } = render(
      <ShellProvider value>
        <MenuShell compactTopPadding>
          <span>Draft content</span>
        </MenuShell>
      </ShellProvider>,
    );

    expect(screen.getByText("Draft content")).toBeInTheDocument();
    expect(container.firstElementChild).toHaveClass("pt-1", "pb-9");
    expect(container.firstElementChild).not.toHaveClass("py-9");
  });

  it("retains standard embedded spacing by default", () => {
    const { container } = render(
      <ShellProvider value>
        <MenuShell>
          <span>Default content</span>
        </MenuShell>
      </ShellProvider>,
    );

    expect(container.firstElementChild).toHaveClass("py-9");
  });
});