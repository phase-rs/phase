import { useState } from "react";

import { useIsMobile } from "./useIsMobile.ts";

/**
 * Where a card choice draws its cards: the phone grid below lg, where the
 * overlay fills the screen, or the prompt's own desktop layout from lg up.
 *
 * `hasSwitched` turns true in the render that first crosses lg after the
 * calling component mounted (a desktop window resized, a tablet turned), and
 * stays true while it stays mounted. Crossing lg swaps one layout for the other
 * and so remounts every card, and the cards' opening entrance must not play
 * again then: the strip's stagger would leave cards far along it blank for
 * seconds.
 */
export function useChoiceCardLayout() {
  const isBelowLg = useIsMobile();
  const [shownBelowLg, setShownBelowLg] = useState(isBelowLg);
  const [hasSwitched, setHasSwitched] = useState(false);

  // Derived-state transition detection: set while rendering, so the render
  // that switches the layout already knows (an effect would run one render
  // too late, after the remounted cards had started their entrance). React
  // renders again with the new state before committing; the caller builds its
  // cards in this pass too, though React discards them, so the switch counts
  // from this pass on and every pass agrees.
  const isSwitching = shownBelowLg !== isBelowLg;
  if (isSwitching) {
    setShownBelowLg(isBelowLg);
    setHasSwitched(true);
  }

  return { isBelowLg, hasSwitched: hasSwitched || isSwitching } as const;
}
