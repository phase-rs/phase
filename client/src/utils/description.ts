/**
 * Render engine-provided ability descriptions for display.
 *
 * The engine uses `~` as the canonical self-reference token (CR 201.5; the
 * gained-ability sub-case is CR 201.5b, the granted-source exception CR 201.5a).
 * Trigger, replacement, and static descriptions reach the client with `~`
 * in place of the source card's name — e.g. "When ~ enters, draw a card."
 * This helper substitutes `~` back to the source's display name for
 * player-facing UI.
 *
 * Replaces EVERY `~` occurrence, unguarded — the engine emits the token only
 * as a self-reference, and one description can carry several ("{T}, Sacrifice
 * ~: ~ deals 1 damage"). Callers relying on that: costLabel.ts, CardPreview,
 * PermanentCard, StackEntry, TargetingOverlay.
 */
export function renderDescription(description: string, sourceName: string): string {
  return description.replace(/~/g, sourceName);
}
