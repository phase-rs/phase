import type { Meta, StoryObj } from "@storybook/react-vite";

import { fn } from "storybook/test";

import { ConfirmDialog } from "./ConfirmDialog.tsx";

/**
 * The confirmation modal for destructive and branching actions. It portals
 * above `ModalPanelShell` so it can stack on a nested flow, traps focus for as
 * long as it is open, and returns focus to whatever opened it.
 *
 * Cancel is the initially focused control, so Enter on a freshly opened dialog
 * never destroys anything.
 */
const meta = {
  title: "UI/ConfirmDialog",
  component: ConfirmDialog,
  // Each story portals a fixed, full-viewport overlay, so a docs page stacking
  // all of them would render four backdrops on top of one another. The stories
  // stay browsable on their own.
  tags: ["!autodocs"],
  parameters: {
    // The dialog fills the viewport and portals to `document.body`, so
    // centering its (empty) placeholder would only offset the backdrop.
    layout: "fullscreen",
  },
  args: {
    open: true,
    title: "Delete deck",
    message: "“Slivers, but worse” and its 100 cards will be removed from this device.",
    confirmLabel: "Delete",
    onConfirm: fn(),
    onCancel: fn(),
  },
  argTypes: {
    tone: { control: "inline-radio", options: ["danger", "primary"] },
    secondaryTone: { control: "inline-radio", options: ["danger", "primary"] },
  },
} satisfies Meta<typeof ConfirmDialog>;

export default meta;

type Story = StoryObj<typeof meta>;

/** The default tone: rose, for an action that loses data. */
export const Danger: Story = {
  args: { tone: "danger" },
};

/** Sky, for a confirmation that only needs deliberateness. */
export const Primary: Story = {
  args: {
    tone: "primary",
    title: "Leave this game?",
    message: "Your opponents keep playing and the seat is forfeited.",
    confirmLabel: "Leave game",
  },
};

/**
 * A second confirm action for flows with two safe answers, such as importing a
 * deck list over the library versus merging into it.
 */
export const TwoConfirmActions: Story = {
  args: {
    title: "Import 42 decks",
    message: "This backup contains decks you already have.",
    confirmLabel: "Replace library",
    tone: "danger",
    secondaryConfirmLabel: "Merge",
    secondaryTone: "primary",
    onSecondaryConfirm: fn(),
  },
};

/** Closed is a real state: the dialog stays mounted and animates on open. */
export const Closed: Story = {
  args: { open: false },
};
