import type { Meta, StoryObj } from "@storybook/react-vite";

import { StoryCardBack } from "../../../../../storybook/mocks/useCardImage.ts";
import { CARD_BACK_URL } from "../../../../services/scryfall.ts";
import { CardVfxGallery } from "./CardVfxGallery.tsx";
import { GALLERY_SCENARIOS } from "./galleryScenarios.ts";

/**
 * Every card animation, replayed on a two-player board through the app's own
 * animation pipeline. Each story plays its engine updates on load; the panel
 * switches between the New (WebGL) and Classic styles, the effect quality and
 * the animation speed, and replays it.
 *
 * Card art comes from Scryfall by name (see `storybook/mocks/`), so the board
 * needs a network connection to show faces.
 */
const meta = {
  title: "Animation/Card VFX",
  component: CardVfxGallery,
  parameters: { layout: "fullscreen" },
  // The New style flips cards over, so it needs the real card back.
  decorators: [
    (Story) => (
      <StoryCardBack.Provider value={CARD_BACK_URL}>
        <Story />
      </StoryCardBack.Provider>
    ),
  ],
  argTypes: {
    scenario: { control: "select", options: Object.keys(GALLERY_SCENARIOS) },
  },
  tags: ["!autodocs"],
} satisfies Meta<typeof CardVfxGallery>;

export default meta;

type Story = StoryObj<typeof meta>;

export const CastAndResolve: Story = { args: { scenario: "castAndResolve" } };
export const PlayLand: Story = { args: { scenario: "playLand" } };
export const Draw: Story = { args: { scenario: "draw" } };
export const OpponentDraw: Story = { args: { scenario: "opponentDraw" } };
export const Discard: Story = { args: { scenario: "discard" } };
export const Mill: Story = { args: { scenario: "mill" } };
export const Destroy: Story = { args: { scenario: "destroy" } };
export const BoardWipe: Story = { args: { scenario: "boardWipe" } };
export const BlackWipe: Story = { args: { scenario: "blackWipe" } };
export const Exile: Story = { args: { scenario: "exile" } };
export const Bounce: Story = { args: { scenario: "bounce" } };
export const MassBounce: Story = { args: { scenario: "massBounce" } };
export const Sacrifice: Story = { args: { scenario: "sacrifice" } };
export const Token: Story = { args: { scenario: "token" } };
export const BurnCreature: Story = { args: { scenario: "burnCreature" } };
export const BurnPlayer: Story = { args: { scenario: "burnPlayer" } };
export const BlueDamage: Story = { args: { scenario: "blueDamage" } };
export const Counterspell: Story = { args: { scenario: "counterspell" } };
export const Combat: Story = { args: { scenario: "combat" } };
export const LifeGain: Story = { args: { scenario: "lifeGain" } };
export const LifeLoss: Story = { args: { scenario: "lifeLoss" } };
export const Counters: Story = { args: { scenario: "counters" } };
