import type { Meta, StoryObj } from "@storybook/react-vite";

import { StoryCardBack } from "../../../storybook/mocks/useCardImage.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import type { VfxQuality } from "../../animation/types.ts";
import { CardVfxGallery, type GalleryScenarioId } from "../animation/cardVfx/gallery/CardVfxGallery.tsx";
import { GALLERY_SCENARIOS } from "../animation/cardVfx/gallery/galleryScenarios.ts";
import type { ManaColor } from "../../adapter/types.ts";
import { ArenaBackground } from "./ArenaBackground.tsx";

interface BackgroundPreviewProps {
  color: ManaColor;
  ambiance: number;
  animated: boolean;
  intensity: number;
  showBoard: boolean;
  scenario: GalleryScenarioId;
  backgroundQuality: VfxQuality;
}

function BackgroundPreview({ color, ambiance, animated, intensity, showBoard, scenario, backgroundQuality }: BackgroundPreviewProps) {
  const background = <ArenaBackground color={color} ambiance={ambiance} animated={animated} intensity={intensity} quality={backgroundQuality} />;
  return showBoard
    ? <CardVfxGallery scenario={scenario} background={background} />
    : <div className="relative h-[100dvh] w-full overflow-hidden">{background}</div>;
}

const meta = {
  title: "Animation/Backgrounds/Mana Arenas",
  component: BackgroundPreview,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story) => <StoryCardBack.Provider value={CARD_BACK_URL}><Story /></StoryCardBack.Provider>,
  ],
  args: { color: "Blue", ambiance: 1, animated: true, intensity: 1, showBoard: false, scenario: "castAndResolve", backgroundQuality: "full" },
  argTypes: {
    color: { control: "select", options: ["White", "Blue", "Black", "Red", "Green"] },
    ambiance: { control: { type: "range", min: 0, max: 2, step: 0.1 } },
    animated: { control: "boolean" },
    intensity: { control: { type: "range", min: 0, max: 2, step: 0.1 } },
    showBoard: { control: "boolean" },
    scenario: { control: "select", options: Object.keys(GALLERY_SCENARIOS) },
    backgroundQuality: { control: "select", options: ["full", "reduced", "minimal"] },
  },
  tags: ["!autodocs"],
} satisfies Meta<typeof BackgroundPreview>;

export default meta;
type Story = StoryObj<typeof meta>;

export const White: Story = { args: { color: "White" } };
export const Blue: Story = {};
export const Black: Story = { args: { color: "Black" } };
export const Red: Story = { args: { color: "Red" } };
export const Green: Story = { args: { color: "Green" } };
export const Static: Story = { args: { color: "Blue", ambiance: 1, animated: false } };
export const WithCards: Story = { args: { showBoard: true } };
export const WithWaterStrike: Story = { args: { showBoard: true, scenario: "blueDamage" } };
