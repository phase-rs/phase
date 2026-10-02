import type { Meta, StoryObj } from "@storybook/react-vite";

import { StoryCardBack } from "../../../storybook/mocks/useCardImage.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import type { VfxQuality } from "../../animation/types.ts";
import { CardVfxGallery, type GalleryScenarioId } from "../animation/cardVfx/gallery/CardVfxGallery.tsx";
import { GALLERY_SCENARIOS } from "../animation/cardVfx/gallery/galleryScenarios.ts";
import { OceanTempleBackground } from "./OceanTempleBackground.tsx";

interface BackgroundPreviewProps {
  animated: boolean;
  intensity: number;
  ambiance: number;
  showBoard: boolean;
  scenario: GalleryScenarioId;
  backgroundQuality: VfxQuality;
}

function BackgroundPreview({ animated, intensity, ambiance, showBoard, scenario, backgroundQuality }: BackgroundPreviewProps) {
  const background = <OceanTempleBackground animated={animated} ambiance={ambiance} intensity={intensity} quality={backgroundQuality} />;
  return showBoard
    ? <CardVfxGallery scenario={scenario} background={background} />
    : <div className="relative h-[100dvh] w-full overflow-hidden">{background}</div>;
}

const meta = {
  title: "Animation/Backgrounds/Ocean Temple",
  component: BackgroundPreview,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story) => <StoryCardBack.Provider value={CARD_BACK_URL}><Story /></StoryCardBack.Provider>,
  ],
  args: { ambiance: 1, animated: true, intensity: 1, showBoard: false, scenario: "castAndResolve", backgroundQuality: "full" },
  argTypes: {
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

export const Ambient: Story = {};
export const Static: Story = { args: { animated: false } };
export const WithCards: Story = { args: { showBoard: true } };
export const WithWaterStrike: Story = { args: { showBoard: true, scenario: "blueDamage" } };
