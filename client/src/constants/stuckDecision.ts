/**
 * How long the same wedged decision must persist before it is treated as a
 * genuine stuck state.
 *
 * The engine can report an empty legal-action set transiently during normal
 * resolution (a decision point that is about to advance on the next store
 * update). Requiring the diagnostic to stay non-null across this debounce
 * window makes a transient blip impossible to surface — only a genuinely hung
 * game (the diagnostic never clears) trips it.
 *
 * Shared by `StuckDecisionToast` (the user-facing toast) and the telemetry
 * `stuck_decision` event so both apply the same persistence gate. Kept in a
 * React-free module so the telemetry install path never imports React.
 */
export const STUCK_DEBOUNCE_MS = 3_000;
