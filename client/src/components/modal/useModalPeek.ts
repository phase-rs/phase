import { useState } from "react";

import { useIsNarrowViewport } from "./DialogHost.tsx";

export interface ModalPeekState {
  /** True while the modal is slid out of the way. */
  peeked: boolean;
  togglePeek: () => void;
  setPeeked: (value: boolean) => void;
  /** Narrow viewport ⇒ slide down (more reachable on phones) rather than right. */
  isNarrow: boolean;
  /** Transform that slides a centered modal to a thin sliver: right on wide
   *  viewports, down on narrow ones. Mirrors the `DialogHost` peek affordance so
   *  collapse muscle-memory is identical for modals rendered outside the host
   *  (e.g. the pre-game mulligan flow). */
  slideTransform: { x: number | string; y: number | string };
}

/**
 * Collapse/peek state for a centered modal that lives OUTSIDE `DialogHost`.
 *
 * `DialogHost` owns this affordance for host-anchored dialogs via its internal
 * peek state + `DialogPeekContext`. Pre-game prompts (mulligan) deliberately
 * render inline rather than through the host, so they reuse the same slide math
 * and tab components (`PeekTab`, `PeekRestoreTab`) through this hook instead.
 */
export function useModalPeek(): ModalPeekState {
  const [peeked, setPeeked] = useState(false);
  const isNarrow = useIsNarrowViewport();
  const slideTransform = peeked
    ? isNarrow
      ? { x: 0, y: "calc(100vh - 64px)" }
      : { x: "calc(100vw - 32px)", y: 0 }
    : { x: 0, y: 0 };

  return {
    peeked,
    togglePeek: () => setPeeked((p) => !p),
    setPeeked,
    isNarrow,
    slideTransform,
  };
}
