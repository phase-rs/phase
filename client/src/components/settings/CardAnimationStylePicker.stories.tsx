import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";

import type { CardAnimationStyle } from "../../animation/types.ts";
import { CardAnimationStylePicker } from "./CardAnimationStylePicker.tsx";

/**
 * The Card Animations setting. Hover (or focus) the tiles to play the same game
 * moment in each style, side by side and looping; click to choose one. On a
 * touch screen a tap starts and stops the preview.
 */
const meta = {
  title: "Settings/Card Animation Style Picker",
  component: CardAnimationStylePicker,
  tags: ["!autodocs"],
  args: { value: "webgl", onChange: () => {} },
  render: (args) => {
    const [value, setValue] = useState<CardAnimationStyle>(args.value);
    return (
      <div className="w-[26rem] max-w-full rounded-[24px] border border-white/10 bg-black/30 p-5">
        <span className="mb-2 block text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
          Card Animations
        </span>
        <CardAnimationStylePicker value={value} onChange={setValue} />
      </div>
    );
  },
} satisfies Meta<typeof CardAnimationStylePicker>;

export default meta;

type Story = StoryObj<typeof meta>;

export const NewSelected: Story = { args: { value: "webgl" } };
export const ClassicSelected: Story = { args: { value: "classic" } };
