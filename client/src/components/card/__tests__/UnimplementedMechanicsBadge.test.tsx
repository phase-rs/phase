import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import { UnimplementedMechanicsBadge } from "../UnimplementedMechanicsBadge.tsx";

const BADGE = "unimplemented-mechanics-badge";

describe("UnimplementedMechanicsBadge", () => {
  afterEach(() => {
    cleanup();
  });

  // The badge owns the emptiness guard so no call site has to repeat it — that
  // duplicated `mechanics.length > 0` check across surfaces is what issue #4711
  // exists to remove. Both "no field" and "empty array" must stay silent: the
  // engine omits the projection entirely for fully-supported cards, and serde
  // can also round-trip it as `[]`.
  it("renders nothing when the engine reports no unimplemented mechanics", () => {
    render(<UnimplementedMechanicsBadge />);
    expect(screen.queryByTestId(BADGE)).not.toBeInTheDocument();
  });

  it("renders nothing for an empty mechanics list", () => {
    render(<UnimplementedMechanicsBadge mechanics={[]} />);
    expect(screen.queryByTestId(BADGE)).not.toBeInTheDocument();
  });

  it("renders the warning when at least one mechanic is unimplemented", () => {
    render(<UnimplementedMechanicsBadge mechanics={["cascade"]} />);
    expect(screen.getByTestId(BADGE)).toBeInTheDocument();
  });

  it("names every unimplemented mechanic in its accessible label, comma-joined", () => {
    // The label is the only place the mechanic names are legible — the glyph is
    // a bare "!", so a screen-reader user and a hover user get the detail here
    // or not at all.
    render(<UnimplementedMechanicsBadge mechanics={["cascade", "storm"]} />);
    const badge = screen.getByTestId(BADGE);
    expect(badge).toHaveAttribute("title", "Unimplemented: cascade, storm");
    expect(badge).toHaveAccessibleName("Unimplemented: cascade, storm");
  });

  it("keeps the amber warning identity in both variants, changing only placement", () => {
    // Shared identity is the point of extracting the component: a stack entry
    // and a hand card must read as the SAME warning, differing only in where it
    // is pinned. `corner` hangs outside the border; `overlay` sits inside the art.
    const { rerender } = render(
      <UnimplementedMechanicsBadge mechanics={["cascade"]} variant="overlay" />,
    );
    const overlay = screen.getByTestId(BADGE).className;
    expect(overlay).toContain("bg-amber-500");
    expect(overlay).toContain("top-0.5");

    rerender(<UnimplementedMechanicsBadge mechanics={["cascade"]} variant="corner" />);
    const corner = screen.getByTestId(BADGE).className;
    expect(corner).toContain("bg-amber-500");
    expect(corner).toContain("-bottom-2");
    expect(corner).toContain("-right-2");
  });

  it("defaults to the overlay variant so existing card surfaces are unchanged", () => {
    render(<UnimplementedMechanicsBadge mechanics={["cascade"]} />);
    expect(screen.getByTestId(BADGE).className).toContain("top-0.5");
  });
});
