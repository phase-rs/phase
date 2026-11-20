import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DialogShell } from "../DialogShell.tsx";
import { DialogHost } from "../DialogHost.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";

describe("DialogShell", () => {
  afterEach(() => {
    cleanup();
  });

  it("renders eyebrow, title, and subtitle", () => {
    render(
      <DialogShell eyebrow="Test Eyebrow" title="Test Title" subtitle="Test Subtitle">
        <div>body</div>
      </DialogShell>,
    );

    expect(screen.getByText("Test Eyebrow")).toBeInTheDocument();
    expect(screen.getByText("Test Title")).toBeInTheDocument();
    expect(screen.getByText("Test Subtitle")).toBeInTheDocument();
    expect(screen.getByText("body")).toBeInTheDocument();
  });

  it("exposes the title as the dialog's accessible name", () => {
    render(
      <DialogShell title="Choose attackers">
        <div>body</div>
      </DialogShell>,
    );

    const dialog = screen.getByRole("dialog", { name: "Choose attackers" });
    const heading = screen.getByRole("heading", { name: "Choose attackers", level: 2 });

    expect(dialog).toHaveAttribute("aria-labelledby", heading.id);
    expect(heading.id).not.toBe("");
  });

  it("renders default eyebrow when none is provided", () => {
    render(
      <DialogShell title="t">
        <div />
      </DialogShell>,
    );
    expect(screen.getByText("Game Choice")).toBeInTheDocument();
  });

  it("calls onClose when the backdrop is clicked", () => {
    const onClose = vi.fn();
    render(
      <DialogShell title="t" onClose={onClose}>
        <div />
      </DialogShell>,
    );
    const backdrop = document.querySelector('[aria-hidden="true"]') as HTMLElement;
    fireEvent.click(backdrop);
    expect(onClose).toHaveBeenCalled();
  });

  it("hides the peek button when rendered outside DialogHost", () => {
    render(
      <DialogShell title="t">
        <div />
      </DialogShell>,
    );
    expect(
      screen.queryByLabelText("Move dialog out of the way"),
    ).not.toBeInTheDocument();
  });

  it("shows the peek button when rendered inside DialogHost", () => {
    useGameStore.setState({
      waitingFor: { type: "ModeChoice", data: { player: 0 } } as never,
    });
    render(
      <DialogHost>
        <DialogShell title="t">
          <div />
        </DialogShell>
      </DialogHost>,
    );
    expect(
      screen.getByLabelText("Move dialog out of the way"),
    ).toBeInTheDocument();
  });

  // Issue #11006: under `viewport-fit=cover` a landscape phone's notch or
  // camera cutout sits inside the left or right safe-area inset, and the
  // optional-effect prompt's two-column dialog nears the screen's full width.
  it("pads each side of the overlay by that side's safe-area inset or the gutter, whichever is larger", () => {
    render(
      <DialogShell title="Rhystic Study - Optional Effect" size="lg">
        <div />
      </DialogShell>,
    );

    const overlay = screen.getByRole("dialog", { name: "Rhystic Study - Optional Effect" })
      .parentElement as HTMLElement;

    // Reach guard: this is the fixed overlay the card sits in.
    expect(overlay).toHaveClass("fixed", "inset-0");
    expect(overlay).toHaveClass(
      "pl-[max(env(safe-area-inset-left),0.5rem)]",
      "pr-[max(env(safe-area-inset-right),0.5rem)]",
      "lg:pl-[max(env(safe-area-inset-left),1rem)]",
      "lg:pr-[max(env(safe-area-inset-right),1rem)]",
    );
    // A fixed side gutter would ignore the insets.
    expect(overlay).not.toHaveClass("px-2");
    expect(overlay).not.toHaveClass("lg:px-4");
  });

  it("renders footer when provided", () => {
    render(
      <DialogShell title="t" footer={<button>Confirm</button>}>
        <div />
      </DialogShell>,
    );
    expect(screen.getByText("Confirm")).toBeInTheDocument();
  });

  // Issue #11000: a prompt taller than the screen was cut off at the top and
  // the bottom, with nothing to scroll. happy-dom does no layout, so these pin
  // the structure that keeps every dialog usable at any height: a card capped
  // at the visible viewport, a pinned header and `footer` slot, and one
  // scrolling body between them.
  describe("a dialog taller than the screen", () => {
    function renderTallDialog() {
      render(
        <DialogShell
          title="Choose two modes"
          onClose={vi.fn()}
          footer={<button type="button">Confirm</button>}
        >
          <ul>
            {Array.from({ length: 60 }, (_, index) => (
              <li key={index}>{`Row ${index}`}</li>
            ))}
          </ul>
        </DialogShell>,
      );

      const heading = screen.getByRole("heading", { name: "Choose two modes" });
      const header = heading.parentElement as HTMLElement;
      const card = header.parentElement as HTMLElement;
      const body = screen.getByRole("list").parentElement as HTMLElement;
      const confirm = screen.getByRole("button", { name: "Confirm" });
      const footer = confirm.parentElement as HTMLElement;
      const close = screen.getByRole("button", { name: "Close" });
      return { heading, header, card, body, confirm, footer, close };
    }

    it("caps the card at the dynamic viewport, less the overlay padding and safe areas", () => {
      const { card } = renderTallDialog();

      expect(card).toHaveClass(
        "flex",
        "flex-col",
        "max-h-[calc(100dvh_-_1rem_-_env(safe-area-inset-top)_-_env(safe-area-inset-bottom))]",
        "lg:max-h-[calc(100dvh_-_3rem_-_env(safe-area-inset-top)_-_env(safe-area-inset-bottom))]",
      );
      // `100vh` is the largest viewport, so a card capped by it runs under the
      // mobile URL bar.
      expect(card.className).not.toContain("100vh");
    });

    it("scrolls only the body, between a pinned header and a pinned footer", () => {
      const { heading, header, card, body, confirm, footer, close } = renderTallDialog();

      expect(Array.from(card.children)).toEqual([header, close, body, footer]);
      expect(header).toHaveClass("shrink-0");
      expect(footer).toHaveClass("shrink-0");
      expect(body).toHaveClass("flex-1", "overflow-y-auto", "overflow-x-hidden");
      // An absolutely positioned child (ReplacementModal's per-row `sr-only`
      // labels) must resolve against the body, or it stretches the card's
      // scroll range and the pinned header and footer scroll away with it.
      expect(body).toHaveClass("relative");

      expect(body).toContainElement(screen.getByText("Row 59"));
      expect(body).not.toContainElement(heading);
      expect(body).not.toContainElement(confirm);
      expect(body).not.toContainElement(close);
    });

    it("scrolls the whole card when the header and footer leave the body less than one row", () => {
      const { card, body } = renderTallDialog();

      // The body stops shrinking at one tap target, and past that the card
      // scrolls the column, footer included.
      expect(body).toHaveClass("min-h-11");
      expect(card).toHaveClass("overflow-y-auto", "overflow-x-hidden", "overscroll-contain");
      // Chromium 144+ applies `overscroll-contain` even to a container with
      // nothing to scroll, so on the body it would keep a swipe from reaching
      // the card.
      expect(body).not.toHaveClass("overscroll-contain");
    });

    it("keeps the header as the drag handle", () => {
      const { header } = renderTallDialog();

      expect(header).toHaveClass("cursor-grab", "touch-none");
    });

    it("centres the card with auto margins, so an oversized card starts at the top", () => {
      renderTallDialog();

      const wrapper = screen.getByRole("dialog", { name: "Choose two modes" });
      const overlay = wrapper.parentElement as HTMLElement;

      // Reach guard: the negative assertion below is about the fixed overlay.
      expect(overlay).toHaveClass("fixed", "inset-0");
      expect(wrapper).toHaveClass("m-auto");
      // `items-center` centres an oversized card past both edges of the screen.
      expect(overlay).not.toHaveClass("items-center");
    });
  });
});
