import {
  defaultFlexLayout,
  type FlexLayoutConfig,
} from "../../stores/preferencesStore.ts";

/** A selectable layout preset (excludes the synthetic "custom"). Each carries a
 *  COMPLETE config — applying one replaces the layout wholesale, including the
 *  opponent HUD. `labelKey` is an i18n key under the `settings` namespace. */
export interface FlexPreset {
  id: "default" | "layout2" | "layout3";
  labelKey: string;
  descriptionKey: string;
  config: FlexLayoutConfig;
}

/** The built-in presets shown in the Flex Layout toolbar/settings picker.
 *  `default` reuses the store's canonical default so the reset target and the
 *  "Default" preset can never drift apart. */
export const FLEX_PRESETS: readonly FlexPreset[] = [
  {
    id: "default",
    labelKey: "flexLayout.presets.default.label",
    descriptionKey: "flexLayout.presets.default.description",
    config: defaultFlexLayout(),
  },
  {
    // Layout 2 — a larger battlefield: smaller hand/opponent bands free
    // vertical space. (Shape-only; no principled use-case rationale.)
    id: "layout2",
    labelKey: "flexLayout.presets.layout2.label",
    descriptionKey: "flexLayout.presets.layout2.description",
    config: {
      gridBands: { top: { pct: 10, pxCap: 80 }, bottom: { pct: 14, pxCap: 120 } },
      widgets: {},
      opponentHudByTableSize: {},
      activePreset: "layout2",
    },
  },
  {
    // Layout 3 — a taller lower third. (Shape-only; no principled rationale.)
    id: "layout3",
    labelKey: "flexLayout.presets.layout3.label",
    descriptionKey: "flexLayout.presets.layout3.description",
    config: {
      gridBands: { top: { pct: 12, pxCap: 100 }, bottom: { pct: 22, pxCap: 180 } },
      widgets: {},
      opponentHudByTableSize: {},
      activePreset: "layout3",
    },
  },
];
