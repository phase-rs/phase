import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ExportDeckControl } from "../ExportDeckControl";

const mocks = vi.hoisted(() => ({ copyText: vi.fn(async () => true) }));

vi.mock("../../../services/copyText", () => ({ copyText: mocks.copyText }));

afterEach(() => {
  cleanup();
  mocks.copyText.mockClear();
});

const boltDeck = { main: [{ name: "Lightning Bolt", count: 4 }], sideboard: [] };

describe("ExportDeckControl", () => {
  it("switches format, copies the shown text and closes", async () => {
    const user = userEvent.setup();
    render(<ExportDeckControl deck={boltDeck} triggerClassName={() => ""} />);

    await user.click(screen.getByRole("button", { name: "Export deck" }));
    const dialog = await screen.findByRole("dialog", { name: "Export Deck" });
    const textbox = within(dialog).getByRole("textbox");
    expect(textbox).toHaveValue("[Main]\n4 Lightning Bolt\n");

    await user.click(within(dialog).getByRole("button", { name: "MTGA" }));
    expect(textbox).toHaveValue("Deck\n4 Lightning Bolt\n");

    await user.click(within(dialog).getByRole("button", { name: "Copy" }));
    expect(mocks.copyText).toHaveBeenCalledWith("Deck\n4 Lightning Bolt\n");
    expect(await within(dialog).findByRole("button", { name: "Copied!" })).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("disables the trigger for an empty deck and styles it through the caller", () => {
    const triggerClassName = vi.fn((disabled: boolean) => (disabled ? "is-disabled" : "is-enabled"));
    const { rerender } = render(
      <ExportDeckControl deck={{ main: [], sideboard: [] }} triggerClassName={triggerClassName} />,
    );

    const trigger = screen.getByRole("button", { name: "Export deck" });
    expect(trigger).toBeDisabled();
    expect(trigger).toHaveClass("is-disabled");
    expect(triggerClassName).toHaveBeenLastCalledWith(true);

    rerender(<ExportDeckControl deck={boltDeck} triggerClassName={triggerClassName} />);
    expect(trigger).toBeEnabled();
    expect(trigger).toHaveClass("is-enabled");
    expect(triggerClassName).toHaveBeenLastCalledWith(false);
  });
});
