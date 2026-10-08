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

  it("adds three flex-height boundaries only for embedded opt-in", () => {
    const { container, rerender } = render(
      <ShellProvider value>
        <MenuShell fillEmbeddedHeight>
          <span>Responsive draft</span>
        </MenuShell>
      </ShellProvider>,
    );

    const outer = container.firstElementChild!;
    const layout = outer.firstElementChild!;
    const children = layout.lastElementChild!;
    expect(outer).toHaveClass("h-full", "min-h-0", "flex-1");
    expect(layout).toHaveClass("min-h-0", "flex-1", "flex", "flex-col");
    expect(children).toHaveClass("min-h-0", "flex-1", "flex", "flex-col");

    rerender(
      <ShellProvider value>
        <MenuShell>
          <span>Default draft</span>
        </MenuShell>
      </ShellProvider>,
    );

    expect(container.firstElementChild).not.toHaveClass("h-full", "min-h-0", "flex-1");
  });
});