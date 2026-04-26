import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { DialogHost } from "../DialogHost.tsx";
import { DialogShell } from "../DialogShell.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import type { WaitingFor } from "../../../adapter/types.ts";

function setWaitingFor(waitingFor: WaitingFor | null) {
  useGameStore.setState({ waitingFor });
}

describe("DialogHost", () => {
  afterEach(() => {
    cleanup();
    setWaitingFor(null);
  });

  it("hides the peek-restore tab while the dialog is visible (un-peeked)", () => {
    setWaitingFor({ type: "ModeChoice", data: {} } as never);
    render(
      <DialogHost>
        <DialogShell title="t">
          <div />
        </DialogShell>
      </DialogHost>,
    );
    expect(screen.queryByLabelText("Restore dialog")).not.toBeInTheDocument();
  });

  it("toggles peek when the shell's peek button is clicked", () => {
    setWaitingFor({ type: "ModeChoice", data: {} } as never);
    render(
      <DialogHost>
        <DialogShell title="t">
          <div />
        </DialogShell>
      </DialogHost>,
    );

    fireEvent.click(screen.getByLabelText("Move dialog out of the way"));
    expect(screen.getByLabelText("Restore dialog")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("Restore dialog"));
    expect(screen.queryByLabelText("Restore dialog")).not.toBeInTheDocument();
  });

  it("does not render the peek tab for non-dialog WaitingFor types", () => {
    setWaitingFor({ type: "Priority", data: { player: 0 } } as never);
    render(
      <DialogHost>
        <DialogShell title="t">
          <div />
        </DialogShell>
      </DialogHost>,
    );
    fireEvent.click(screen.getByLabelText("Move dialog out of the way"));
    expect(screen.queryByLabelText("Restore dialog")).not.toBeInTheDocument();
  });

  it("resets peek to false when WaitingFor changes (regression)", () => {
    setWaitingFor({ type: "ModeChoice", data: {} } as never);
    render(
      <DialogHost>
        <DialogShell title="t">
          <div />
        </DialogShell>
      </DialogHost>,
    );

    fireEvent.click(screen.getByLabelText("Move dialog out of the way"));
    expect(screen.getByLabelText("Restore dialog")).toBeInTheDocument();

    act(() => {
      setWaitingFor({ type: "ReplacementChoice", data: {} } as never);
    });
    expect(screen.queryByLabelText("Restore dialog")).not.toBeInTheDocument();
  });
});
