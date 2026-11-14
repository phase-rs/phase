import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { DraftSteps } from "../DraftSteps";

describe("DraftSteps", () => {
  it("renders responsive drafting steps as a compact word strip", () => {
    const { container } = render(<DraftSteps phase="drafting" compact />);

    const steps = container.querySelector('[data-draft-steps="compact"]');
    expect(steps).toHaveClass("py-0.5");
    expect(screen.getByText("Choose Set")).toBeInTheDocument();
    expect(screen.getByText("Draft")).toHaveAttribute("aria-current", "step");
    expect(screen.getByText("Draft")).toHaveClass("bg-emerald-400", "text-slate-950");
    expect(screen.getByText("Build Deck")).toBeInTheDocument();
    expect(screen.getByText("Play")).toBeInTheDocument();
    expect(container.querySelectorAll("svg")).toHaveLength(0);
  });

  it("uses thin right arrows for phone drafting and deckbuilding", () => {
    const rendered = render(<DraftSteps phase="drafting" compact arrowSeparators />);
    const steps = within(rendered.container);

    expect(rendered.container.querySelectorAll("[data-step-arrow]")).toHaveLength(3);
    expect(rendered.container).not.toHaveTextContent("|");
    expect(steps.getByText("Draft")).toHaveAttribute("aria-current", "step");

    rendered.rerender(<DraftSteps phase="deckbuilding" compact arrowSeparators />);
    expect(steps.getByText("Build Deck")).toHaveAttribute("aria-current", "step");
    expect(steps.getByText("Build Deck")).toHaveClass("bg-emerald-400");
  });

  it("retains the numbered desktop presentation", () => {
    const { container } = render(<DraftSteps phase="drafting" />);

    expect(container.querySelector('[data-draft-steps="compact"]')).not.toBeInTheDocument();
    expect(screen.getByText("2")).toHaveClass("rounded-full", "bg-emerald-400");
  });
});