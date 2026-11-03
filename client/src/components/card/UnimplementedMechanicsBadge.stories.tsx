import type { Meta, StoryObj } from "@storybook/react-vite";

import { UnimplementedMechanicsBadge } from "./UnimplementedMechanicsBadge.tsx";

/**
 * Amber `!` shown on a card whose printed abilities include mechanics the
 * engine does not implement yet. Display-only: the engine computes
 * `unimplemented_mechanics` and this forwards it verbatim.
 *
 * Both variants are absolutely positioned, so each story anchors the badge to a
 * stand-in card surface the way its real call sites do.
 */
const meta = {
  title: "Card/UnimplementedMechanicsBadge",
  component: UnimplementedMechanicsBadge,
  args: {
    mechanics: ["Hideaway"],
  },
  argTypes: {
    variant: { control: "inline-radio", options: ["overlay", "corner"] },
  },
  decorators: [
    (Story) => (
      <div className="relative h-40 w-28 rounded-lg border border-white/20 bg-slate-800">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof UnimplementedMechanicsBadge>;

export default meta;

type Story = StoryObj<typeof meta>;

/** Pinned inside the art of a hand or battlefield card, deliberately small. */
export const Overlay: Story = {
  args: { variant: "overlay" },
};

/**
 * Hung outside the border of a stack entry, matching the ×N and status pills
 * already on that surface. Bottom-right is the stack entry's only free corner.
 */
export const Corner: Story = {
  args: { variant: "corner" },
};

/** The badge is the single authority on when the warning shows: no mechanics,
 *  no badge, so no call site repeats the emptiness guard. */
export const NoMechanics: Story = {
  args: { mechanics: [] },
};

/** Every unimplemented mechanic is listed in the tooltip and accessible name. */
export const SeveralMechanics: Story = {
  args: { mechanics: ["Hideaway", "Bestow", "Fateseal"] },
};
