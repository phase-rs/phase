import type { CSSProperties } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";

import { CardArtFallback } from "./CardArtFallback.tsx";
import { getBevelBorderStyle } from "./cardFrame.ts";

/**
 * The text tile a card degrades to when it has no renderable art — a token with
 * no paper printing, or an image that failed to load. Both board renderers
 * route their artless cases here so an unillustrated permanent stays
 * identifiable instead of rendering as a blank rectangle.
 */
const meta = {
  title: "Card/CardArtFallback",
  component: CardArtFallback,
  args: {
    name: "Banana",
    oracleText: "{T}, Sacrifice this token: Add {R} or {G}. You gain 2 life.",
    className: "w-[var(--card-w)] h-[var(--card-h)] rounded-lg",
    style: getBevelBorderStyle(["Colorless"]),
  },
  argTypes: {
    variant: { control: "inline-radio", options: ["fullCard", "artCrop"] },
  },
  decorators: [
    (Story) => (
      // Matches the CardImage catalog scale: the responsive `--card-w` clamp
      // bottoms out inside a Storybook panel.
      <div style={{ "--card-size-scale": 1.75 } as CSSProperties}>
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof CardArtFallback>;

export default meta;

type Story = StoryObj<typeof meta>;

/** Full-card surfaces have room for the Oracle text under the name. */
export const FullCard: Story = {
  args: { variant: "fullCard" },
};

/**
 * The art-crop tile is a fraction of a card's height, so it shows the name
 * alone — Oracle text at that scale would push the name out of the box.
 */
export const ArtCrop: Story = {
  args: {
    variant: "artCrop",
    className: "w-[var(--art-crop-w)] h-[var(--art-crop-h)] rounded-lg",
  },
};

/** Mana symbols in the Oracle text render as glyphs, not as `{R}` literals. */
export const ManaSymbolsInText: Story = {
  args: {
    name: "Treasure",
    oracleText: "{T}, Sacrifice this token: Add one mana of any color.",
    style: getBevelBorderStyle(["Colorless"]),
  },
};

/** A long name truncates rather than wrapping past the tile. */
export const LongName: Story = {
  args: {
    name: "Rhys the Redeemed, Emissary of the Wren-Kin Assembly",
    oracleText: undefined,
    style: getBevelBorderStyle(["Green", "White"]),
  },
};
