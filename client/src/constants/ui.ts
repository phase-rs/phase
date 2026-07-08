/** Tilt angle in degrees for attacking/blocking creatures. */
export const COMBAT_TILT_DEGREES = 15;

/** Default animation duration in ms for UI transitions. */
export const DEFAULT_ANIMATION_DURATION_MS = 200;

/**
 * Game-surface stacking layers. Use these for board, prompt, modal, and debug
 * surfaces instead of raw z-* classes.
 *
 * Keep board-choice chrome above ordinary rails, but below DialogHost: the host
 * owns prompt controls such as targeting/crew/sacrifice Confirm buttons while
 * using pointer-events:none so battlefield clicks still pass through.
 */
export const GAME_Z = {
  board: 10,
  hudRail: 30,
  boardChoiceGrid: 35,
  dialogHost: 40,
  modal: 50,
  floatingOverlay: 60,
  nestedDialog: 70,
  debugPanel: 9999,
} as const;

export const GAME_Z_LAYER = {
  board: "z-10",
  hudRail: "z-30",
  boardChoiceGrid: "z-[35]",
  dialogHost: "z-40",
  modal: "z-50",
  floatingOverlay: "z-[60]",
  nestedDialog: "z-[70]",
  debugPanel: "z-[9999]",
} as const;
