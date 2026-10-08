import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConfirmDialog } from "../ConfirmDialog";
import { MenuSelect } from "../MenuSelect";
import { ModalPanelShell } from "../ModalPanelShell";

const externalElements: HTMLElement[] = [];

afterEach(() => {
  cleanup();
  for (const element of externalElements.splice(0)) element.remove();
});

function appendExternalButton(): HTMLButtonElement {
  const button = document.createElement("button");
  document.body.append(button);
  externalElements.push(button);
  return button;
}

function AutoFocusedPortal() {
  const ref = useRef<HTMLButtonElement>(null);
  useLayoutEffect(() => ref.current?.focus(), []);
  return createPortal(
    <button ref={ref} type="button">
      Successor action
    </button>,
    document.body,
  );
}

function OverlappingConfirmations() {
  const [firstOpen, setFirstOpen] = useState(false);
  const [secondOpen, setSecondOpen] = useState(false);
  return (
    <ModalPanelShell title="Edit deck" onClose={vi.fn()}>
      <button type="button" onClick={() => setFirstOpen(true)}>
        Open first confirmation
      </button>
      <ConfirmDialog
        open={firstOpen}
        title="First confirmation"
        message="The first confirmation remains open."
        confirmLabel="Open second confirmation"
        onConfirm={() => setSecondOpen(true)}
        onCancel={() => setFirstOpen(false)}
      />
      <ConfirmDialog
        open={secondOpen}
        title="Second confirmation"
        message="Return to the first confirmation."
        confirmLabel="Finish"
        onConfirm={vi.fn()}
        onCancel={() => setSecondOpen(false)}
      />
    </ModalPanelShell>
  );
}

function shell({
  open = true,
  onClose = vi.fn(),
  children = <input aria-label="Deck name" />,
}: {
  open?: boolean;
  onClose?: () => void;
  children?: React.ReactNode;
} = {}) {
  return (
    <ModalPanelShell open={open} title="Edit deck" onClose={onClose}>
      {children}
    </ModalPanelShell>
  );
}

describe("ModalPanelShell", () => {
  it("exposes its title as the modal dialog's accessible name", () => {
    render(shell());

    const dialog = screen.getByRole("dialog", { name: "Edit deck" });
    const heading = screen.getByRole("heading", { name: "Edit deck", level: 2 });

    expect(dialog).toHaveAttribute("aria-modal", "true");
    expect(dialog).toHaveAttribute("aria-labelledby", heading.id);
    expect(heading.id).not.toBe("");
  });

  it("initially focuses the dialog container instead of its first input", async () => {
    render(shell());

    await waitFor(() =>
      expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus(),
    );
    expect(screen.getByRole("textbox", { name: "Deck name" })).not.toHaveFocus();
  });

  it("moves focus from the dialog container to its first focusable control on Tab", () => {
    render(shell());

    const dialog = screen.getByRole("dialog", { name: "Edit deck" });
    const firstControl = within(dialog).getByRole("button", { name: "Close Edit deck" });

    fireEvent.keyDown(dialog, { key: "Tab" });

    expect(firstControl).toHaveFocus();
  });

  it("wraps forward focus from the last control to the first control", () => {
    render(
      shell({
        children: (
          <>
            <input aria-label="Deck name" />
            <button type="button">Save</button>
          </>
        ),
      }),
    );

    const dialog = screen.getByRole("dialog", { name: "Edit deck" });
    const firstControl = within(dialog).getByRole("button", { name: "Close Edit deck" });
    const lastControl = within(dialog).getByRole("button", { name: "Save" });
    lastControl.focus();

    fireEvent.keyDown(lastControl, { key: "Tab" });

    expect(firstControl).toHaveFocus();
  });

  it("wraps reverse focus from the first control to the last control", () => {
    render(
      shell({
        children: (
          <>
            <input aria-label="Deck name" />
            <button type="button">Save</button>
          </>
        ),
      }),
    );

    const dialog = screen.getByRole("dialog", { name: "Edit deck" });
    const firstControl = within(dialog).getByRole("button", { name: "Close Edit deck" });
    const lastControl = within(dialog).getByRole("button", { name: "Save" });
    firstControl.focus();

    fireEvent.keyDown(firstControl, { key: "Tab", shiftKey: true });

    expect(lastControl).toHaveFocus();
  });

  it("recovers inside the modal when a focused descendant is removed", async () => {
    function RemovingControl() {
      const [visible, setVisible] = useState(true);
      return visible ? (
        <button type="button" onClick={() => setVisible(false)}>
          Remove focused control
        </button>
      ) : (
        <p>Control removed</p>
      );
    }

    render(shell({ children: <RemovingControl /> }));
    const control = screen.getByRole("button", {
      name: "Remove focused control",
    });
    control.focus();
    fireEvent.click(control);

    const dialog = screen.getByRole("dialog", { name: "Edit deck" });
    await waitFor(() =>
      expect(
        within(dialog).getByRole("button", { name: "Close Edit deck" }),
      ).toHaveFocus(),
    );
    expect(document.body).not.toHaveFocus();
  });

  it("does not steal an explicit successor focus after an unrelated mutation", async () => {
    const successor = appendExternalButton();
    successor.textContent = "Successor";
    const view = render(
      shell({
        children: (
          <>
            <button type="button">Remembered control</button>
            <p>Before mutation</p>
          </>
        ),
      }),
    );
    await waitFor(() =>
      expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus(),
    );
    screen.getByRole("button", { name: "Remembered control" }).focus();
    successor.focus();

    view.rerender(
      shell({
        children: (
          <>
            <button type="button">Remembered control</button>
            <p>After mutation</p>
          </>
        ),
      }),
    );

    await waitFor(() => expect(screen.getByText("After mutation")).toBeInTheDocument());
    expect(successor).toHaveFocus();
  });

  it("keeps the complete positive tabIndex sequence inside the modal", async () => {
    const user = userEvent.setup();
    const outside = appendExternalButton();
    render(
      shell({
        children: (
          <>
            <button type="button" tabIndex={2}>Second</button>
            <button type="button" tabIndex={1}>First</button>
          </>
        ),
      }),
    );

    const dialog = screen.getByRole("dialog", { name: "Edit deck" });
    const first = within(dialog).getByRole("button", { name: "First" });
    const second = within(dialog).getByRole("button", { name: "Second" });
    const close = within(dialog).getByRole("button", { name: "Close Edit deck" });
    dialog.focus();

    await user.tab();
    expect(first).toHaveFocus();

    await user.tab();
    expect(second).toHaveFocus();

    await user.tab();
    expect(close).toHaveFocus();
    expect(outside).not.toHaveFocus();
  });

  it("skips CSS-hidden controls when wrapping focus", () => {
    render(
      shell({
        children: (
          <>
            <button type="button">Save</button>
            <div style={{ display: "none" }}>
              <button type="button">Hidden action</button>
            </div>
          </>
        ),
      }),
    );

    const dialog = screen.getByRole("dialog", { name: "Edit deck" });
    const firstControl = within(dialog).getByRole("button", { name: "Close Edit deck" });
    const lastControl = within(dialog).getByRole("button", { name: "Save" });
    lastControl.focus();
    fireEvent.keyDown(lastControl, { key: "Tab" });

    expect(firstControl).toHaveFocus();
  });

  it("skips controls disabled by a fieldset when wrapping focus", () => {
    render(
      shell({
        children: (
          <>
            <button type="button">Save</button>
            <fieldset disabled>
              <button type="button">Disabled action</button>
            </fieldset>
          </>
        ),
      }),
    );

    const dialog = screen.getByRole("dialog", { name: "Edit deck" });
    const firstControl = within(dialog).getByRole("button", { name: "Close Edit deck" });
    const lastControl = within(dialog).getByRole("button", { name: "Save" });
    lastControl.focus();
    fireEvent.keyDown(lastControl, { key: "Tab" });

    expect(firstControl).toHaveFocus();
  });

  it("skips embedded browsing contexts when containing Tab", () => {
    render(
      shell({
        children: (
          <>
            <iframe title="Embedded frame" tabIndex={0} />
            <object title="Embedded object" tabIndex={0} />
            <embed title="Embedded content" tabIndex={0} />
            <button type="button">Save</button>
          </>
        ),
      }),
    );

    const dialog = screen.getByRole("dialog", { name: "Edit deck" });
    const close = within(dialog).getByRole("button", { name: "Close Edit deck" });
    close.focus();

    fireEvent.keyDown(close, { key: "Tab" });

    expect(within(dialog).getByRole("button", { name: "Save" })).toHaveFocus();
  });

  it("uses only the checked member of a radio group as its tab stop", () => {
    render(
      shell({
        children: (
          <>
            <button type="button">Save</button>
            <label>
              First option
              <input type="radio" name="mode" defaultChecked />
            </label>
            <label>
              Second option
              <input type="radio" name="mode" />
            </label>
          </>
        ),
      }),
    );

    const dialog = screen.getByRole("dialog", { name: "Edit deck" });
    fireEvent.keyDown(dialog, { key: "Tab", shiftKey: true });

    expect(within(dialog).getByRole("radio", { name: "First option" })).toHaveFocus();
  });

  it("lets a descendant consume Escape and Tab before the shell", () => {
    const onClose = vi.fn();
    render(
      shell({
        onClose,
        children: (
          <button
            type="button"
            onKeyDown={(event) => event.preventDefault()}
          >
            Embedded editor
          </button>
        ),
      }),
    );

    const editor = screen.getByRole("button", { name: "Embedded editor" });
    editor.focus();
    fireEvent.keyDown(editor, { key: "Escape" });
    fireEvent.keyDown(editor, { key: "Tab" });

    expect(onClose).not.toHaveBeenCalled();
    expect(editor).toHaveFocus();
  });

  it("keeps focus on the dialog when it has no enabled focusable controls", () => {
    render(shell({ children: <p>Nothing to edit.</p> }));

    const dialog = screen.getByRole("dialog", { name: "Edit deck" });
    const closeButton = within(dialog).getByRole("button", { name: "Close Edit deck" });
    closeButton.setAttribute("disabled", "");

    fireEvent.keyDown(dialog, { key: "Tab" });

    expect(dialog).toHaveFocus();
  });

  it("closes once on Escape without propagating the keydown to window", () => {
    const onClose = vi.fn();
    const onWindowKeyDown = vi.fn();
    window.addEventListener("keydown", onWindowKeyDown);

    try {
      render(shell({ onClose }));
      const dialog = screen.getByRole("dialog", { name: "Edit deck" });

      fireEvent.keyDown(dialog, { key: "Escape" });

      expect(onClose).toHaveBeenCalledTimes(1);
      expect(onWindowKeyDown).not.toHaveBeenCalled();
    } finally {
      window.removeEventListener("keydown", onWindowKeyDown);
    }
  });

  it("ignores modal keyboard handling while an IME composition is active", () => {
    const onClose = vi.fn();
    render(shell({ onClose }));
    const input = screen.getByRole("textbox", { name: "Deck name" });
    input.focus();

    fireEvent.keyDown(input, { key: "Escape", isComposing: true });
    fireEvent.keyDown(input, { key: "Escape", keyCode: 229 });

    expect(screen.getByRole("dialog", { name: "Edit deck" })).toBeInTheDocument();
    expect(input).toHaveFocus();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("does not handle Escape after it closes", async () => {
    const onClose = vi.fn();
    const view = render(shell({ onClose }));
    await waitFor(() =>
      expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus(),
    );
    const dialog = screen.getByRole("dialog", { name: "Edit deck" });

    view.rerender(shell({ open: false, onClose }));
    fireEvent.keyDown(dialog, { key: "Escape" });

    expect(onClose).not.toHaveBeenCalled();
  });

  it("leaves the parent open when a nested portaled dialog handles Escape", async () => {
    const onClose = vi.fn();
    const onCancel = vi.fn();
    const nestedShell = (confirmOpen: boolean) => (
      <ModalPanelShell title="Edit deck" onClose={onClose}>
        <button type="button">Delete deck</button>
        <ConfirmDialog
          open={confirmOpen}
          title="Delete deck?"
          message="This cannot be undone."
          confirmLabel="Delete"
          onConfirm={vi.fn()}
          onCancel={onCancel}
        />
      </ModalPanelShell>
    );
    const view = render(nestedShell(false));
    await waitFor(() =>
      expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus(),
    );

    view.rerender(nestedShell(true));
    const nestedDialog = await screen.findByRole("alertdialog", { name: "Delete deck?" });
    fireEvent.keyDown(nestedDialog, { key: "Escape" });

    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "Edit deck" })).toBeInTheDocument();
  });

  it("ignores keys from an unregistered React portal", () => {
    const onClose = vi.fn();
    render(
      shell({
        onClose,
        children: createPortal(
          <button type="button">Unregistered portal action</button>,
          document.body,
        ),
      }),
    );

    const portalAction = screen.getByRole("button", {
      name: "Unregistered portal action",
    });
    portalAction.focus();

    expect(fireEvent.keyDown(portalAction, { key: "Escape" })).toBe(true);
    expect(fireEvent.keyDown(portalAction, { key: "Tab" })).toBe(true);
    expect(portalAction).toHaveFocus();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("contains composing Tab from a filterable portaled select", () => {
    const onClose = vi.fn();
    const outside = appendExternalButton();
    render(
      shell({
        onClose,
        children: (
          <>
            <MenuSelect
              label="Board style"
              items={[{ value: "classic", label: "Classic" }]}
              onSelect={vi.fn()}
              filterable
              filterPlaceholder="Search board styles"
            />
            <button type="button">Save preferences</button>
          </>
        ),
      }),
    );

    const trigger = screen.getByRole("button", { name: "Board style" });
    fireEvent.click(trigger);
    const filter = screen.getByRole("textbox", { name: "Search board styles" });
    expect(filter).toHaveFocus();

    fireEvent.keyDown(filter, { key: "Tab", isComposing: true });

    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save preferences" })).toHaveFocus();
    expect(outside).not.toHaveFocus();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("contains composing Shift+Tab from a filterable portaled select", () => {
    render(
      shell({
        children: (
          <>
            <MenuSelect
              label="Board style"
              items={[{ value: "classic", label: "Classic" }]}
              onSelect={vi.fn()}
              filterable
              filterPlaceholder="Search board styles"
            />
            <button type="button">Save preferences</button>
          </>
        ),
      }),
    );

    const trigger = screen.getByRole("button", { name: "Board style" });
    fireEvent.click(trigger);
    fireEvent.keyDown(screen.getByRole("textbox", { name: "Search board styles" }), {
      key: "Tab",
      shiftKey: true,
      keyCode: 229,
    });

    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(
      within(screen.getByRole("dialog", { name: "Edit deck" })).getByRole(
        "button",
        { name: "Close Edit deck" },
      ),
    ).toHaveFocus();
  });

  it("lets a portaled select own Escape and restores its trigger", () => {
    const onClose = vi.fn();
    render(
      shell({
        onClose,
        children: (
          <MenuSelect
            label="Board style"
            items={[{ value: "classic", label: "Classic" }]}
            onSelect={vi.fn()}
          />
        ),
      }),
    );

    const trigger = screen.getByRole("button", { name: "Board style" });
    fireEvent.click(trigger);
    fireEvent.keyDown(screen.getByRole("option", { name: "Classic" }), {
      key: "Escape",
    });

    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("recovers within an open portal branch when its focused option is removed", async () => {
    const modal = (items: Array<{ value: string; label: string }>) =>
      shell({
        children: (
          <MenuSelect
            label="Board style"
            items={items}
            onSelect={vi.fn()}
          />
        ),
      });
    const view = render(
      modal([
        { value: "classic", label: "Classic" },
        { value: "modern", label: "Modern" },
      ]),
    );
    fireEvent.click(screen.getByRole("button", { name: "Board style" }));
    expect(screen.getByRole("option", { name: "Classic" })).toHaveFocus();

    view.rerender(modal([{ value: "modern", label: "Modern" }]));

    await waitFor(() =>
      expect(screen.getByRole("option", { name: "Modern" })).toHaveFocus(),
    );
    expect(screen.getByRole("listbox")).toBeInTheDocument();
  });

  it("remembers a focused portal option across a reorder before its removal", async () => {
    const modal = (items: Array<{ value: string; label: string }>) =>
      shell({
        children: (
          <MenuSelect
            label="Board style"
            items={items}
            onSelect={vi.fn()}
          />
        ),
      });
    const classic = { value: "classic", label: "Classic" };
    const modern = { value: "modern", label: "Modern" };
    const view = render(modal([classic, modern]));
    fireEvent.click(screen.getByRole("button", { name: "Board style" }));
    const focused = screen.getByRole("option", { name: "Classic" });
    expect(focused).toHaveFocus();

    view.rerender(modal([modern, classic]));
    expect(screen.getByRole("option", { name: "Classic" })).toBe(focused);
    expect(focused).toHaveFocus();

    view.rerender(modal([modern]));
    await waitFor(() =>
      expect(screen.getByRole("option", { name: "Modern" })).toHaveFocus(),
    );
  });

  it("restores a portaled select trigger after an option is chosen", () => {
    const onSelect = vi.fn();
    render(
      shell({
        children: (
          <MenuSelect
            label="Board style"
            items={[{ value: "classic", label: "Classic" }]}
            onSelect={onSelect}
          />
        ),
      }),
    );

    const trigger = screen.getByRole("button", { name: "Board style" });
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("option", { name: "Classic" }));

    expect(onSelect).toHaveBeenCalledWith("classic");
    expect(trigger).toHaveFocus();
  });

  it("keeps Tab in a nested confirmation and restores its opener on Escape", async () => {
    const onClose = vi.fn();

    function NestedConfirmation() {
      const [confirmOpen, setConfirmOpen] = useState(false);
      return (
        <ModalPanelShell title="Edit deck" onClose={onClose}>
          <button type="button" onClick={() => setConfirmOpen(true)}>
            Delete deck
          </button>
          <button type="button">Save deck</button>
          <ConfirmDialog
            open={confirmOpen}
            title="Delete deck?"
            message="This cannot be undone."
            confirmLabel="Delete"
            onConfirm={vi.fn()}
            onCancel={() => setConfirmOpen(false)}
          />
        </ModalPanelShell>
      );
    }

    render(<NestedConfirmation />);
    const opener = screen.getByRole("button", { name: "Delete deck" });
    opener.focus();
    fireEvent.click(opener);

    const confirmation = await screen.findByRole("alertdialog", {
      name: "Delete deck?",
    });
    const cancel = within(confirmation).getByRole("button", { name: "Cancel" });
    const confirm = within(confirmation).getByRole("button", { name: "Delete" });
    await waitFor(() => expect(cancel).toHaveFocus());

    confirm.focus();
    fireEvent.keyDown(confirm, { key: "Tab" });
    expect(cancel).toHaveFocus();

    fireEvent.keyDown(cancel, { key: "Tab", shiftKey: true });
    expect(confirm).toHaveFocus();

    opener.focus();
    fireEvent.keyDown(opener, { key: "Tab" });
    expect(cancel).toHaveFocus();

    fireEvent.keyDown(cancel, { key: "Escape" });
    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(opener).toHaveFocus();
    expect(screen.getByRole("dialog", { name: "Edit deck" })).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("restores an overlapping confirmation to its opener in the still-open sibling", async () => {
    render(<OverlappingConfirmations />);
    fireEvent.click(
      screen.getByRole("button", { name: "Open first confirmation" }),
    );
    const first = await screen.findByRole("alertdialog", {
      name: "First confirmation",
    });
    const secondOpener = within(first).getByRole("button", {
      name: "Open second confirmation",
    });
    secondOpener.focus();
    fireEvent.click(secondOpener);

    const second = await screen.findByRole("alertdialog", {
      name: "Second confirmation",
    });
    const secondCancel = within(second).getByRole("button", { name: "Cancel" });
    await waitFor(() => expect(secondCancel).toHaveFocus());
    fireEvent.keyDown(secondCancel, { key: "Escape" });

    await waitFor(() =>
      expect(
        screen.queryByRole("alertdialog", { name: "Second confirmation" }),
      ).not.toBeInTheDocument(),
    );
    expect(secondOpener).toHaveFocus();
    expect(first).toBeInTheDocument();
  });

  it("falls back within a still-open sibling when its captured opener becomes unavailable", async () => {
    render(<OverlappingConfirmations />);
    fireEvent.click(
      screen.getByRole("button", { name: "Open first confirmation" }),
    );
    const first = await screen.findByRole("alertdialog", {
      name: "First confirmation",
    });
    const secondOpener = within(first).getByRole("button", {
      name: "Open second confirmation",
    });
    secondOpener.focus();
    fireEvent.click(secondOpener);

    const second = await screen.findByRole("alertdialog", {
      name: "Second confirmation",
    });
    const secondCancel = within(second).getByRole("button", { name: "Cancel" });
    await waitFor(() => expect(secondCancel).toHaveFocus());
    secondOpener.setAttribute("disabled", "");
    fireEvent.keyDown(secondCancel, { key: "Escape" });

    await waitFor(() =>
      expect(
        screen.queryByRole("alertdialog", { name: "Second confirmation" }),
      ).not.toBeInTheDocument(),
    );
    expect(within(first).getByRole("button", { name: "Cancel" })).toHaveFocus();
  });

  it("preserves the original return path through a same-commit successor scope", async () => {
    function SuccessiveConfirmations() {
      const [phase, setPhase] = useState<"first" | "second" | null>(null);
      return (
        <ModalPanelShell title="Edit deck" onClose={vi.fn()}>
          <button type="button" onClick={() => setPhase("first")}>
            Launch confirmation flow
          </button>
          <ConfirmDialog
            open={phase === "first"}
            title="First step"
            message="Continue to the next confirmation."
            confirmLabel="Continue"
            onConfirm={() => setPhase("second")}
            onCancel={() => setPhase(null)}
          />
          <ConfirmDialog
            open={phase === "second"}
            title="Second step"
            message="Finish the confirmation flow."
            confirmLabel="Finish"
            onConfirm={vi.fn()}
            onCancel={() => setPhase(null)}
          />
        </ModalPanelShell>
      );
    }

    render(<SuccessiveConfirmations />);
    const launcher = screen.getByRole("button", {
      name: "Launch confirmation flow",
    });
    launcher.focus();
    fireEvent.click(launcher);
    const first = await screen.findByRole("alertdialog", { name: "First step" });
    const continueButton = within(first).getByRole("button", { name: "Continue" });
    continueButton.focus();
    fireEvent.click(continueButton);

    const second = await screen.findByRole("alertdialog", { name: "Second step" });
    const secondCancel = within(second).getByRole("button", { name: "Cancel" });
    await waitFor(() => expect(secondCancel).toHaveFocus());
    fireEvent.keyDown(secondCancel, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(launcher).toHaveFocus();
  });

  it("tries a successor's explicit target when its inherited return path becomes invalid", async () => {
    function SuccessorWithFallbackCandidate() {
      const [phase, setPhase] = useState<"first" | "second" | null>(null);
      const [launcherHidden, setLauncherHidden] = useState(false);
      const fallbackRef = useRef<HTMLButtonElement>(null);
      return (
        <ModalPanelShell title="Edit deck" onClose={vi.fn()}>
          <button
            type="button"
            hidden={launcherHidden}
            onClick={() => setPhase("first")}
          >
            Launch confirmation flow
          </button>
          <button ref={fallbackRef} type="button">
            Stable fallback
          </button>
          <ConfirmDialog
            open={phase === "first"}
            title="First step"
            message="Continue to the next confirmation."
            confirmLabel="Continue"
            onConfirm={() => {
              setLauncherHidden(true);
              setPhase("second");
            }}
            onCancel={() => setPhase(null)}
          />
          <ConfirmDialog
            open={phase === "second"}
            title="Second step"
            message="Finish the confirmation flow."
            confirmLabel="Finish"
            returnFocusRef={fallbackRef}
            onConfirm={vi.fn()}
            onCancel={() => setPhase(null)}
          />
        </ModalPanelShell>
      );
    }

    render(<SuccessorWithFallbackCandidate />);
    const launcher = screen.getByRole("button", {
      name: "Launch confirmation flow",
    });
    launcher.focus();
    fireEvent.click(launcher);
    const first = await screen.findByRole("alertdialog", { name: "First step" });
    const continueButton = within(first).getByRole("button", { name: "Continue" });
    continueButton.focus();
    fireEvent.click(continueButton);

    const second = await screen.findByRole("alertdialog", { name: "Second step" });
    const secondCancel = within(second).getByRole("button", { name: "Cancel" });
    await waitFor(() => expect(secondCancel).toHaveFocus());
    fireEvent.keyDown(secondCancel, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("button", { name: "Stable fallback" })).toHaveFocus();
  });

  it("keeps simultaneous restoration records constrained to their selected top layer", async () => {
    function SimultaneousPendingScopes() {
      const [siblingOpen, setSiblingOpen] = useState(true);
      const [nestedOpen, setNestedOpen] = useState(false);
      const closePendingScopes = () => {
        if (document.activeElement instanceof HTMLElement) {
          document.activeElement.blur();
        }
        setSiblingOpen(false);
        setNestedOpen(false);
      };
      return (
        <ModalPanelShell title="Root panel" onClose={vi.fn()}>
          <ModalPanelShell
            open={siblingOpen}
            title="Closing sibling"
            onClose={() => setSiblingOpen(false)}
          >
            <p>Closing sibling content</p>
          </ModalPanelShell>
          <ModalPanelShell title="Remaining sibling" onClose={vi.fn()}>
            <button type="button">Earlier action</button>
            <button type="button" onClick={() => setNestedOpen(true)}>
              Open nested confirmation
            </button>
            <ConfirmDialog
              open={nestedOpen}
              title="Closing nested confirmation"
              message="Close with the unrelated sibling."
              confirmLabel="Close both"
              onConfirm={closePendingScopes}
              onCancel={() => setNestedOpen(false)}
            />
          </ModalPanelShell>
        </ModalPanelShell>
      );
    }

    render(<SimultaneousPendingScopes />);
    const nestedOpener = screen.getByRole("button", {
      name: "Open nested confirmation",
    });
    nestedOpener.focus();
    fireEvent.click(nestedOpener);
    const nested = await screen.findByRole("alertdialog", {
      name: "Closing nested confirmation",
    });
    await waitFor(() =>
      expect(within(nested).getByRole("button", { name: "Cancel" })).toHaveFocus(),
    );
    fireEvent.click(within(nested).getByRole("button", { name: "Close both" }));

    await waitFor(() =>
      expect(
        screen.queryByRole("alertdialog", {
          name: "Closing nested confirmation",
        }),
      ).not.toBeInTheDocument(),
    );
    expect(nestedOpener).toHaveFocus();
    expect(screen.getByRole("button", { name: "Earlier action" })).not.toHaveFocus();
  });

  it("inherits the matching predecessor when a body-focused successor mounts with unrelated pending work", async () => {
    function CombinedSuccessorCleanup() {
      const [unrelatedOpen, setUnrelatedOpen] = useState(true);
      const [phase, setPhase] = useState<"first" | "second" | null>(null);
      const replaceAndCloseUnrelated = () => {
        if (document.activeElement instanceof HTMLElement) {
          document.activeElement.blur();
        }
        setUnrelatedOpen(false);
        setPhase("second");
      };
      return (
        <ModalPanelShell title="Root panel" onClose={vi.fn()}>
          <ModalPanelShell
            open={unrelatedOpen}
            title="Unrelated sibling"
            onClose={() => setUnrelatedOpen(false)}
          >
            <p>Unrelated content</p>
          </ModalPanelShell>
          <ModalPanelShell title="Surviving sibling" onClose={vi.fn()}>
            <button type="button">Earlier action</button>
            <button type="button" onClick={() => setPhase("first")}>
              Launch nested flow
            </button>
            <ConfirmDialog
              open={phase === "first"}
              title="First nested step"
              message="Replace this confirmation."
              confirmLabel="Continue"
              onConfirm={replaceAndCloseUnrelated}
              onCancel={() => setPhase(null)}
            />
            <ConfirmDialog
              open={phase === "second"}
              title="Second nested step"
              message="Return to the original launcher."
              confirmLabel="Finish"
              onConfirm={vi.fn()}
              onCancel={() => setPhase(null)}
            />
          </ModalPanelShell>
        </ModalPanelShell>
      );
    }

    render(<CombinedSuccessorCleanup />);
    const nestedOpener = screen.getByRole("button", {
      name: "Launch nested flow",
    });
    nestedOpener.focus();
    fireEvent.click(nestedOpener);
    const first = await screen.findByRole("alertdialog", {
      name: "First nested step",
    });
    await waitFor(() =>
      expect(within(first).getByRole("button", { name: "Cancel" })).toHaveFocus(),
    );
    fireEvent.click(within(first).getByRole("button", { name: "Continue" }));

    const second = await screen.findByRole("alertdialog", {
      name: "Second nested step",
    });
    const secondCancel = within(second).getByRole("button", { name: "Cancel" });
    await waitFor(() => expect(secondCancel).toHaveFocus());
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "Unrelated sibling" }),
      ).not.toBeInTheDocument(),
    );
    fireEvent.keyDown(secondCancel, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(nestedOpener).toHaveFocus();
    expect(screen.getByRole("button", { name: "Earlier action" })).not.toHaveFocus();
  });

  it("traps and restores focus for a standalone confirmation", async () => {
    function StandaloneConfirmation() {
      const [open, setOpen] = useState(false);
      return (
        <>
          <button type="button" onClick={() => setOpen(true)}>
            Remove profile
          </button>
          <ConfirmDialog
            open={open}
            title="Remove profile?"
            message="This cannot be undone."
            confirmLabel="Remove"
            onConfirm={vi.fn()}
            onCancel={() => setOpen(false)}
          />
        </>
      );
    }

    render(<StandaloneConfirmation />);
    const opener = screen.getByRole("button", { name: "Remove profile" });
    opener.focus();
    fireEvent.click(opener);

    const confirmation = await screen.findByRole("alertdialog", {
      name: "Remove profile?",
    });
    const cancel = within(confirmation).getByRole("button", { name: "Cancel" });
    const confirm = within(confirmation).getByRole("button", { name: "Remove" });
    await waitFor(() => expect(cancel).toHaveFocus());

    confirm.focus();
    fireEvent.keyDown(confirm, { key: "Tab" });
    expect(cancel).toHaveFocus();

    fireEvent.keyDown(cancel, { key: "Escape" });
    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(opener).toHaveFocus();
  });

  it("forwards an explicit return-focus ref through a pointer-opened confirmation", async () => {
    const staleFocus = appendExternalButton();
    staleFocus.focus();

    function ExplicitConfirmationReturn() {
      const [open, setOpen] = useState(false);
      const openerRef = useRef<HTMLButtonElement>(null);
      return (
        <>
          <button
            ref={openerRef}
            type="button"
            onClick={() => setOpen(true)}
          >
            Remove profile
          </button>
          <ConfirmDialog
            open={open}
            title="Remove profile?"
            message="This cannot be undone."
            confirmLabel="Remove"
            returnFocusRef={openerRef}
            onConfirm={vi.fn()}
            onCancel={() => setOpen(false)}
          />
        </>
      );
    }

    render(<ExplicitConfirmationReturn />);
    const opener = screen.getByRole("button", { name: "Remove profile" });
    expect(staleFocus).toHaveFocus();
    fireEvent.click(opener);
    const confirmation = await screen.findByRole("alertdialog", {
      name: "Remove profile?",
    });
    const cancel = within(confirmation).getByRole("button", { name: "Cancel" });
    await waitFor(() => expect(cancel).toHaveFocus());

    fireEvent.keyDown(cancel, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(opener).toHaveFocus();
  });

  it("restores the outer opener when the panel closes with a confirmation active", async () => {
    const outerOpener = appendExternalButton();
    outerOpener.focus();
    const modal = (open: boolean) => (
      <ModalPanelShell open={open} title="Edit deck" onClose={vi.fn()}>
        <button type="button">Delete deck</button>
        <ConfirmDialog
          open
          title="Delete deck?"
          message="This cannot be undone."
          confirmLabel="Delete"
          onConfirm={vi.fn()}
          onCancel={vi.fn()}
        />
      </ModalPanelShell>
    );

    const view = render(modal(true));
    const confirmation = await screen.findByRole("alertdialog", {
      name: "Delete deck?",
    });
    await waitFor(() =>
      expect(within(confirmation).getByRole("button", { name: "Cancel" })).toHaveFocus(),
    );

    view.rerender(modal(false));

    expect(outerOpener).toHaveFocus();
  });

  it("falls back to the parent panel when a confirmation opener disconnects", async () => {
    function DisconnectedOpenerConfirmation() {
      const [confirmOpen, setConfirmOpen] = useState(false);
      const [showOpener, setShowOpener] = useState(true);
      return (
        <ModalPanelShell title="Edit deck" onClose={vi.fn()}>
          {showOpener && (
            <button type="button" onClick={() => setConfirmOpen(true)}>
              Delete deck
            </button>
          )}
          <ConfirmDialog
            open={confirmOpen}
            title="Delete deck?"
            message="This cannot be undone."
            confirmLabel="Delete"
            onConfirm={vi.fn()}
            onCancel={() => {
              setConfirmOpen(false);
              setShowOpener(false);
            }}
          />
        </ModalPanelShell>
      );
    }

    render(<DisconnectedOpenerConfirmation />);
    const opener = screen.getByRole("button", { name: "Delete deck" });
    opener.focus();
    fireEvent.click(opener);
    const confirmation = await screen.findByRole("alertdialog", {
      name: "Delete deck?",
    });
    const cancel = within(confirmation).getByRole("button", { name: "Cancel" });
    await waitFor(() => expect(cancel).toHaveFocus());

    fireEvent.keyDown(cancel, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(screen.queryByRole("button", { name: "Delete deck" })).not.toBeInTheDocument();
    expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus();
  });

  it.each(["disabled", "hidden"] as const)(
    "falls back to the parent panel when a confirmation opener becomes %s",
    async (unavailableState) => {
      function UnavailableOpenerConfirmation() {
        const [confirmOpen, setConfirmOpen] = useState(false);
        const [openerUnavailable, setOpenerUnavailable] = useState(false);
        return (
          <ModalPanelShell title="Edit deck" onClose={vi.fn()}>
            <button
              type="button"
              disabled={
                unavailableState === "disabled" && openerUnavailable
              }
              hidden={unavailableState === "hidden" && openerUnavailable}
              onClick={() => setConfirmOpen(true)}
            >
              Delete deck
            </button>
            <ConfirmDialog
              open={confirmOpen}
              title="Delete deck?"
              message="This cannot be undone."
              confirmLabel="Delete"
              onConfirm={vi.fn()}
              onCancel={() => {
                setConfirmOpen(false);
                setOpenerUnavailable(true);
              }}
            />
          </ModalPanelShell>
        );
      }

      render(<UnavailableOpenerConfirmation />);
      const opener = screen.getByRole("button", { name: "Delete deck" });
      opener.focus();
      fireEvent.click(opener);
      const confirmation = await screen.findByRole("alertdialog", {
        name: "Delete deck?",
      });
      const cancel = within(confirmation).getByRole("button", { name: "Cancel" });
      await waitFor(() => expect(cancel).toHaveFocus());

      fireEvent.keyDown(cancel, { key: "Escape" });

      await waitFor(() =>
        expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
      );
      expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus();
    },
  );

  it("falls back to the parent panel when a connected opener refuses focus", async () => {
    function RefusingOpenerConfirmation() {
      const [confirmOpen, setConfirmOpen] = useState(false);
      return (
        <ModalPanelShell title="Edit deck" onClose={vi.fn()}>
          <button type="button" onClick={() => setConfirmOpen(true)}>
            Delete deck
          </button>
          <ConfirmDialog
            open={confirmOpen}
            title="Delete deck?"
            message="This cannot be undone."
            confirmLabel="Delete"
            onConfirm={vi.fn()}
            onCancel={() => setConfirmOpen(false)}
          />
        </ModalPanelShell>
      );
    }

    render(<RefusingOpenerConfirmation />);
    const opener = screen.getByRole("button", { name: "Delete deck" });
    opener.focus();
    fireEvent.click(opener);
    const confirmation = await screen.findByRole("alertdialog", {
      name: "Delete deck?",
    });
    const cancel = within(confirmation).getByRole("button", { name: "Cancel" });
    await waitFor(() => expect(cancel).toHaveFocus());
    vi.spyOn(opener, "focus").mockImplementation(() => undefined);

    fireEvent.keyDown(cancel, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus();
  });

  it("restores focus to a connected opener when the modal closes", async () => {
    const opener = appendExternalButton();
    opener.focus();

    const view = render(shell());
    await waitFor(() =>
      expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus(),
    );

    view.rerender(shell({ open: false }));

    expect(opener).toHaveFocus();
  });

  it("uses an explicit return-focus ref when pointer activation leaves the opener unfocused", async () => {
    function ExplicitReturnFocusPanel() {
      const [open, setOpen] = useState(false);
      const openerRef = useRef<HTMLButtonElement>(null);
      return (
        <>
          <button
            ref={openerRef}
            type="button"
            onClick={() => setOpen(true)}
          >
            Open editor
          </button>
          <ModalPanelShell
            open={open}
            title="Edit deck"
            returnFocusRef={openerRef}
            onClose={() => setOpen(false)}
          >
            <p>Editor content</p>
          </ModalPanelShell>
        </>
      );
    }

    render(<ExplicitReturnFocusPanel />);
    const opener = screen.getByRole("button", { name: "Open editor" });
    expect(opener).not.toHaveFocus();
    fireEvent.click(opener);
    const dialog = await screen.findByRole("dialog", { name: "Edit deck" });
    await waitFor(() => expect(dialog).toHaveFocus());

    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "Edit deck" })).not.toBeInTheDocument(),
    );
    expect(opener).toHaveFocus();
  });

  it("tries the captured active element when an explicit root target is invalid", async () => {
    function InvalidExplicitReturnPanel() {
      const [open, setOpen] = useState(false);
      const invalidRef = useRef<HTMLButtonElement>(null);
      return (
        <>
          <button type="button" onClick={() => setOpen(true)}>
            Open editor
          </button>
          <button ref={invalidRef} type="button" disabled>
            Disabled preferred target
          </button>
          <ModalPanelShell
            open={open}
            title="Edit deck"
            returnFocusRef={invalidRef}
            onClose={() => setOpen(false)}
          >
            <p>Editor content</p>
          </ModalPanelShell>
        </>
      );
    }

    render(<InvalidExplicitReturnPanel />);
    const capturedOpener = screen.getByRole("button", { name: "Open editor" });
    capturedOpener.focus();
    fireEvent.click(capturedOpener);
    const dialog = await screen.findByRole("dialog", { name: "Edit deck" });
    await waitFor(() => expect(dialog).toHaveFocus());

    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "Edit deck" })).not.toBeInTheDocument(),
    );
    expect(capturedOpener).toHaveFocus();
  });

  it("uses the latest explicit return-focus ref without re-registering an open scope", async () => {
    function ResponsiveReturnPanel({ compact }: { compact: boolean }) {
      const [open, setOpen] = useState(false);
      const desktopRef = useRef<HTMLButtonElement>(null);
      const compactRef = useRef<HTMLButtonElement>(null);
      return (
        <>
          <button
            ref={desktopRef}
            type="button"
            hidden={compact}
            onClick={() => setOpen(true)}
          >
            Desktop editor launcher
          </button>
          <button
            ref={compactRef}
            type="button"
            hidden={!compact}
            onClick={() => setOpen(true)}
          >
            Compact editor launcher
          </button>
          <ModalPanelShell
            open={open}
            title="Edit deck"
            returnFocusRef={compact ? compactRef : desktopRef}
            onClose={() => setOpen(false)}
          >
            <p>Editor content</p>
          </ModalPanelShell>
        </>
      );
    }

    const view = render(<ResponsiveReturnPanel compact={false} />);
    fireEvent.click(
      screen.getByRole("button", { name: "Desktop editor launcher" }),
    );
    const dialog = await screen.findByRole("dialog", { name: "Edit deck" });
    await waitFor(() => expect(dialog).toHaveFocus());

    view.rerender(<ResponsiveReturnPanel compact />);
    expect(
      screen.getByRole("button", { name: "Compact editor launcher" }),
    ).toBeVisible();
    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "Edit deck" })).not.toBeInTheDocument(),
    );
    expect(
      screen.getByRole("button", { name: "Compact editor launcher" }),
    ).toHaveFocus();
  });

  it("restores focus to a connected opener when the modal unmounts", async () => {
    const opener = appendExternalButton();
    opener.focus();

    const view = render(shell());
    await waitFor(() =>
      expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus(),
    );

    view.unmount();

    expect(opener).toHaveFocus();
  });

  it("does not steal focus from a successor surface when it closes", async () => {
    const opener = appendExternalButton();
    const successor = appendExternalButton();
    opener.focus();

    const view = render(shell());
    await waitFor(() =>
      expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus(),
    );

    successor.focus();
    view.rerender(shell({ open: false }));

    expect(successor).toHaveFocus();
  });

  it("does not override a successor portal focused in the closing commit", async () => {
    const opener = appendExternalButton();
    opener.focus();

    function ReplacingModal() {
      const [open, setOpen] = useState(true);
      return open ? (
        <ModalPanelShell title="Edit deck" onClose={() => setOpen(false)}>
          <button type="button" onClick={() => setOpen(false)}>
            Continue
          </button>
        </ModalPanelShell>
      ) : (
        <AutoFocusedPortal />
      );
    }

    render(<ReplacingModal />);
    await waitFor(() =>
      expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus(),
    );
    const continueButton = screen.getByRole("button", { name: "Continue" });
    continueButton.focus();
    fireEvent.click(continueButton);

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Successor action" })).toHaveFocus(),
    );
    expect(opener).not.toHaveFocus();
  });

  it("does not steal focus or throw when its opener disconnected before unmount", async () => {
    const opener = appendExternalButton();
    const fallback = appendExternalButton();
    opener.focus();

    const view = render(shell());
    await waitFor(() =>
      expect(screen.getByRole("dialog", { name: "Edit deck" })).toHaveFocus(),
    );

    opener.remove();
    fallback.focus();

    expect(() => view.unmount()).not.toThrow();
    expect(fallback).toHaveFocus();
  });
});
