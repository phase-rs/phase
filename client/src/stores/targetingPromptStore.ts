import { create } from "zustand";

/**
 * Where the targeting prompt's instruction block sits on screen. The prompt
 * renders on the dialog-host layer, above board widgets such as the stack
 * panel, so a widget it would cover reads these edges and keeps clear of the
 * block instead (#11004). `TargetingOverlay` measures and publishes the edges
 * while the block is mounted, and resets them when the block leaves.
 */
interface TargetingPromptState {
  /** The block's top edge in viewport pixels. 0 while no prompt is on screen. */
  topPx: number;
  /** The block's bottom edge in viewport pixels, an expanded description
   *  included. 0 while no prompt is on screen: an edge at the top of the
   *  viewport keeps nothing clear. */
  bottomPx: number;
  setEdges: (edges: { topPx: number; bottomPx: number }) => void;
}

export const useTargetingPromptStore = create<TargetingPromptState>((set) => ({
  topPx: 0,
  bottomPx: 0,
  setEdges: (edges) => set(edges),
}));
