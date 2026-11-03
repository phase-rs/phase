import type { Meta, StoryObj } from "@storybook/react-vite";

import { LoyaltyBadge } from "./LoyaltyBadge.tsx";

/**
 * One visual contract for both loyalty facts a planeswalker shows: its current
 * total (CR 306.5c) and the cost of a loyalty ability (CR 606.4). The layered
 * mana-font silhouette keeps the printed marker shape while the text overlay
 * stays legible for values the font has no numeral for.
 */
const meta = {
  title: "UI/LoyaltyBadge",
  component: LoyaltyBadge,
  args: {
    amount: 4,
    kind: "total",
  },
  argTypes: {
    kind: { control: "inline-radio", options: ["cost", "total"] },
    size: { control: "inline-radio", options: ["default", "battlefield"] },
  },
} satisfies Meta<typeof LoyaltyBadge>;

export default meta;

type Story = StoryObj<typeof meta>;

/** Starting loyalty, drawn on the shield the printed card uses. */
export const Total: Story = {};

/** Activation costs carry an explicit sign and their own shield shape. */
export const Costs: Story = {
  render: (args) => (
    <div className="flex items-center gap-4">
      <LoyaltyBadge {...args} kind="cost" amount={2} />
      <LoyaltyBadge {...args} kind="cost" amount={0} />
      <LoyaltyBadge {...args} kind="cost" amount={-3} />
      <LoyaltyBadge {...args} kind="cost" amount={-11} />
    </div>
  ),
};

/**
 * An accepted counter-growth loop (CR 732.2a) makes the total unbounded. The
 * `kind === "total"` guard makes an `∞` *cost* structurally unrepresentable,
 * whatever a caller passes.
 */
export const Unbounded: Story = {
  args: { kind: "total", isUnbounded: true, amount: 9001 },
};

/** The battlefield renders the badge a step larger than menus and tooltips. */
export const Sizes: Story = {
  render: (args) => (
    <div className="flex items-center gap-4">
      <LoyaltyBadge {...args} size="default" />
      <LoyaltyBadge {...args} size="battlefield" />
    </div>
  ),
};

/**
 * The loyalty shield has a concave top contour, so the rim needs an extra
 * upward shadow to read as silver at the notch.
 */
export const ReinforcedTopRim: Story = {
  render: (args) => (
    <div className="flex items-center gap-4">
      <LoyaltyBadge {...args} reinforcedTopRim={false} />
      <LoyaltyBadge {...args} reinforcedTopRim />
    </div>
  ),
  args: { size: "battlefield" },
};
