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
      waitingFor: { type: "ModeChoice", data: {} } as never,
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

  it("renders footer when provided", () => {
    render(
      <DialogShell title="t" footer={<button>Confirm</button>}>
        <div />
      </DialogShell>,
    );
    expect(screen.getByText("Confirm")).toBeInTheDocument();
  });
});
