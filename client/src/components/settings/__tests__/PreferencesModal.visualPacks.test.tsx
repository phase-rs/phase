import { useEffect, useState } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { PreferencesModal } from "../PreferencesModal.tsx";

const manager = vi.hoisted(() => ({ mounts: vi.fn(), unmounts: vi.fn() }));
vi.mock("../visual-packs/VisualPackManager.tsx", () => ({
  VisualPackManager: () => {
    useEffect(() => {
      manager.mounts();
      return manager.unmounts;
    }, []);
    return <section aria-label="Offline visual catalog">Offline visual catalog test manager</section>;
  },
}));

function Harness() {
  const [open, setOpen] = useState(true);
  return open ? <PreferencesModal onClose={() => setOpen(false)} /> : null;
}

describe("PreferencesModal visual packs", () => {
  afterEach(() => {
    cleanup();
    manager.mounts.mockReset();
    manager.unmounts.mockReset();
  });

  it("mounts the manager only in Data before existing backup and telemetry controls", () => {
    render(<Harness />);
    expect(screen.queryByLabelText("Offline visual catalog")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /^Data$/i }));
    const visual = screen.getByLabelText("Offline visual catalog");
    const exportButton = screen.getByRole("button", { name: /Export backup/i });
    expect(visual.compareDocumentPosition(exportButton) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByRole("button", { name: /Import backup/i })).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /anonymous crash/i })).toBeInTheDocument();
    expect(manager.mounts).toHaveBeenCalledTimes(1);
  });

  it("disposes the manager when leaving Data or closing the modal", () => {
    const view = render(<Harness />);
    fireEvent.click(screen.getByRole("button", { name: /^Data$/i }));
    fireEvent.click(screen.getByRole("button", { name: /^Gameplay$/i }));
    expect(manager.unmounts).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: /^Data$/i }));
    view.unmount();
    expect(manager.mounts).toHaveBeenCalledTimes(2);
    expect(manager.unmounts).toHaveBeenCalledTimes(2);
  });
});
