import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { TextPromptDialog } from "../TextPromptDialog";

describe("TextPromptDialog", () => {
  afterEach(() => {
    cleanup();
  });

  it("submits trimmed input on confirm", async () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <TextPromptDialog
        open
        title="New folder"
        label="Folder name:"
        confirmLabel="Create"
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );

    await userEvent.type(screen.getByLabelText("Folder name:"), "  Archetypes  ");
    await userEvent.click(screen.getByRole("button", { name: "Create" }));

    expect(onConfirm).toHaveBeenCalledWith("Archetypes");
    expect(onCancel).not.toHaveBeenCalled();
  });

  it("cancels without submitting", async () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <TextPromptDialog
        open
        title="New folder"
        label="Folder name:"
        confirmLabel="Create"
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );

    await userEvent.type(screen.getByLabelText("Folder name:"), "Draft");
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(onCancel).toHaveBeenCalled();
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("keeps confirm disabled until non-blank input", async () => {
    render(
      <TextPromptDialog
        open
        title="New folder"
        label="Folder name:"
        confirmLabel="Create"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Create" })).toBeDisabled();
    await userEvent.type(screen.getByLabelText("Folder name:"), "   ");
    expect(screen.getByRole("button", { name: "Create" })).toBeDisabled();
  });
});
