import type { CSSProperties } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";

import { CardImage } from "./CardImage.tsx";

/**
 * The app's full-card renderer: hand, modals, and every selection surface that
 * shows a card at printed proportions.
 *
 * Art is resolved by a story double for `useCardImage` (see
 * `storybook/mocks/`), which asks Scryfall for the image by name instead of
 * reading the gitignored lookup maps the real hook uses. That keeps the
 * component's three render branches reachable from ordinary props: art in
 * flight draws the skeleton, art resolved draws the `<img>`, and a name
 * Scryfall does not know draws `CardArtFallback`.
 */
const meta = {
  title: "Card/CardImage",
  component: CardImage,
  decorators: [
    (Story) => (
      // `--card-w` is viewport-relative and bottoms out at its `clamp()` floor
      // in a Storybook panel, which is much narrower than a game board. Scale
      // it up so the catalog shows the card at a reviewable size.
      <div style={{ "--card-size-scale": 1.75 } as CSSProperties}>
        <Story />
      </div>
    ),
  ],
  args: {
    cardName: "Lightning Bolt",
    size: "normal",
  },
  argTypes: {
    size: { control: "inline-radio", options: ["small", "normal", "large"] },
    colors: {
      control: "check",
      options: ["White", "Blue", "Black", "Red", "Green", "Colorless"],
    },
    faceDownCause: {
      control: "select",
      options: ["Manifest", "Morph", "Cloak", "Disguise", "TurnedFaceDown"],
    },
  },
} satisfies Meta<typeof CardImage>;

export default meta;

type Story = StoryObj<typeof meta>;

export const Default: Story = {};

/** A colored bevel replaces the neutral border, matching the card's frame. */
export const ColoredBevel: Story = {
  args: {
    cardName: "Llanowar Elves",
    colors: ["Green"],
  },
};

/** Two-color cards blend the bevel from the first color into the last. */
export const GoldBevel: Story = {
  args: {
    cardName: "Lightning Helix",
    colors: ["Red", "White"],
  },
};

/** Board rendering rotates the card 90°, the way a tapped permanent sits. */
export const Tapped: Story = {
  args: {
    cardName: "Llanowar Elves",
    colors: ["Green"],
    tapped: true,
  },
};

/**
 * Selection modals show permanents upright, so tapped state has to be readable
 * without the rotation. `tapIndicator` adds the {T} pip instead.
 */
export const TapIndicator: Story = {
  args: {
    cardName: "Llanowar Elves",
    colors: ["Green"],
    tapIndicator: true,
  },
};

/** Amber `!` warning for printed abilities the engine does not implement yet. */
export const UnimplementedMechanics: Story = {
  args: {
    cardName: "Shelldock Isle",
    unimplementedMechanics: ["Hideaway"],
    colors: ["Blue"],
  },
};

/**
 * A face-down permanent renders the card back. The marker tokens the app draws
 * for Morph and Manifest are token printings resolved through the Scryfall
 * lookup maps, which the story double does not carry — so this story shows the
 * generic back, which is also what the app renders for `TurnedFaceDown`.
 */
export const FaceDown: Story = {
  args: {
    faceDown: true,
    faceDownCause: "TurnedFaceDown",
  },
};

/** Every face past the front resolves to the card's back half. */
export const BackFace: Story = {
  args: {
    cardName: "Delver of Secrets // Insectile Aberration",
    faceIndex: 1,
    colors: ["Blue"],
  },
};

/**
 * When art cannot be resolved the card degrades to a text tile rather than a
 * blank rectangle, so an artless card stays identifiable. Tokens with no paper
 * printing hit this path in the real app.
 */
export const NoArtAvailable: Story = {
  args: {
    cardName: "Banana",
    isToken: true,
    oracleText: "{T}, Sacrifice this token: Add {R} or {G}. You gain 2 life.",
  },
};

/** Every frame bevel next to each other, where a wrong blend shows up. */
export const FrameColors: Story = {
  render: (args) => (
    <div className="flex flex-wrap items-end justify-center gap-4">
      <CardImage {...args} cardName="Serra Angel" colors={["White"]} />
      <CardImage {...args} cardName="Counterspell" colors={["Blue"]} />
      <CardImage {...args} cardName="Dark Ritual" colors={["Black"]} />
      <CardImage {...args} cardName="Lightning Bolt" colors={["Red"]} />
      <CardImage {...args} cardName="Llanowar Elves" colors={["Green"]} />
      <CardImage {...args} cardName="Sol Ring" colors={["Colorless"]} />
    </div>
  ),
};
