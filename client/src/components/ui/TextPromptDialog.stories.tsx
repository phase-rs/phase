import type { Meta, StoryObj } from "@storybook/react-vite";

import { fn } from "storybook/test";

import { TextPromptDialog } from "./TextPromptDialog.tsx";

/**
 * The site-styled replacement for `window.prompt()`: one line of text, Cancel
 * and confirm. Opening it focuses and selects the field, Escape cancels, and
 * confirm stays disabled until the value has non-whitespace content.
 */
const meta = {
  title: "UI/TextPromptDialog",
  component: TextPromptDialog,
  // Each story portals a fixed, full-viewport overlay, so a docs page stacking
  // all of them would render four backdrops on top of one another. The stories
  // stay browsable on their own.
  tags: ["!autodocs"],
  parameters: {
    // Portals to `document.body` and covers the viewport, like ConfirmDialog.
    layout: "fullscreen",
  },
  args: {
    open: true,
    title: "New folder",
    label: "Folder name",
    confirmLabel: "Create",
    onConfirm: fn(),
    onCancel: fn(),
  },
} satisfies Meta<typeof TextPromptDialog>;

export default meta;

type Story = StoryObj<typeof meta>;

/**
 * Empty is the opening state, and confirm is disabled until something is typed
 * rather than letting an empty submit fail.
 */
export const Empty: Story = {};

/** Renaming pre-fills the current value and selects it, so typing replaces it. */
export const Rename: Story = {
  args: {
    title: "Rename folder",
    initialValue: "Commander",
    confirmLabel: "Rename",
  },
};

/** A placeholder is set only where it adds a hint the label cannot carry. */
export const WithPlaceholder: Story = {
  args: {
    title: "Join a lobby",
    label: "Lobby code",
    placeholder: "ABCD-1234",
    confirmLabel: "Join",
    maxLength: 9,
  },
};

/** Closed is a real state: the dialog stays mounted and animates on open. */
export const Closed: Story = {
  args: { open: false },
};
