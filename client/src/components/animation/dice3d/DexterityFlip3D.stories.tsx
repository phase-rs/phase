import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { DexterityFlip3D } from "./DexterityFlip3D.tsx";

// The overlay's verdict accents (DiceRollOverlay's COIN_WON / COIN_LOST).
const KEPT = "52,211,153";
const SACRIFICED = "251,113,133";

/**
 * A dexterity flip (Orcish Paratroopers, Chaos Orb): a permanent's art tile is
 * dropped from above and tumbles to land as the engine reported — which side is
 * up and how many complete turns it made. The caller owns the verdict and
 * passes its accent; the component only animates.
 *
 * The art is fetched from Scryfall's `art_crop` for Orcish Paratroopers, the
 * image the battlefield shows by default. In the client it comes from
 * `useCardImage`.
 */
const meta = {
  title: "Animation/DexterityFlip3D",
  component: DexterityFlip3D,
  args: {
    result: { landing: "faceUp", fullTurns: 2 },
    artSrc: "https://api.scryfall.com/cards/named?exact=Orcish%20Paratroopers&format=image&version=art_crop",
    accentRgb: KEPT,
    speedMultiplier: 1,
    onSettle: fn(),
  },
  decorators: [
    (Story) => (
      <div className="flex items-center justify-center bg-[radial-gradient(circle_at_center,rgba(2,6,23,0.55),rgba(2,6,23,0.86)_70%)] p-6">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof DexterityFlip3D>;

export default meta;

type Story = StoryObj<typeof meta>;

/** Two complete turns, face up: Orcish Paratroopers stays. */
export const LandsFaceUp: Story = {};

/** One complete turn, then the last half turn: face down, so it is sacrificed. */
export const LandsFaceDown: Story = {
  args: { result: { landing: "faceDown", fullTurns: 1 }, accentRgb: SACRIFICED },
};

/** Face up but never turned over completely: also sacrificed. */
export const NeverTurnedOver: Story = {
  args: { result: { landing: "faceUp", fullTurns: 0 }, accentRgb: SACRIFICED },
};

/** No art (still loading, or none to load): the face is a plain slate panel. */
export const WithoutArt: Story = {
  args: { artSrc: null },
};

/** Quarter speed, for checking the tumble frame by frame. */
export const Slow: Story = {
  args: { speedMultiplier: 4 },
};
