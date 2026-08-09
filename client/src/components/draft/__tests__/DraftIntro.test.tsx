import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";

import { DraftIntro } from "../DraftIntro";

describe("DraftIntro", () => {
  it("shows the draft's configured pack count and pack size", () => {
    render(
      <DraftIntro
        mode="quick"
        packCount={4}
        cardsPerPack={12}
        onContinue={vi.fn()}
      />,
    );

    expect(screen.getByText("You'll open 4 packs of 12 cards each")).toBeInTheDocument();
  });
});
