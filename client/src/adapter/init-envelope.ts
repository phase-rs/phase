/**
 * Single authority for reading the engine's initialize-error envelope.
 *
 * `initialize_game` and `initialize_multiplayer_host_game` both return
 * `{ error: true, reasons: [...] }` on failure, optionally carrying a typed
 * discriminator (`cedh_bracket_violation`, `engine_occupied`) so callers raise
 * a typed error instead of matching on a raw string substring.
 *
 * Four call sites consume that envelope — the engine worker's two initialize
 * cases and the main-thread fallback's two — and every one of them has to tell
 * the typed refusals apart from an ordinary deck-validation failure, or it
 * mislabels an occupied-engine refusal as "Deck validation failed: …". The
 * classification lives here once; each caller only decides how to raise it
 * (worker response vs. thrown `AdapterError`).
 *
 * Deliberately dependency-free: `engine-worker.ts` imports it into the worker
 * bundle, which must not pull in the main-thread adapter types.
 */

export type InitFailureKind = "bracketViolation" | "engineOccupied" | "deckValidation";

export interface InitFailure {
  kind: InitFailureKind;
  /** Message to surface to the caller. */
  message: string;
  /** Raw engine reasons, for callers that surface them without a prefix. */
  reasons: string[];
}

/**
 * User-facing text for an occupied-engine refusal, worded for both directions
 * the engine guard refuses in: a hosted game starting on top of a live local
 * game, and a local game starting on top of a hosted one. On a
 * memory-constrained device those share one engine worker, so "your current
 * game" is accurate either way.
 */
const ENGINE_OCCUPIED_MESSAGE =
  "Finish or leave your current game before starting a new one.";

/**
 * Returns the typed failure an initialize call reported, or `null` when the
 * result is a normal `ActionResult`.
 */
export function classifyInitFailure(result: unknown): InitFailure | null {
  if (!result || typeof result !== "object" || !("error" in result) || !result.error) {
    return null;
  }
  const envelope = result as {
    reasons?: string[];
    cedh_bracket_violation?: boolean;
    engine_occupied?: boolean;
  };
  const reasons = envelope.reasons ?? [];
  if (envelope.engine_occupied) {
    return { kind: "engineOccupied", message: ENGINE_OCCUPIED_MESSAGE, reasons };
  }
  const message = `Deck validation failed: ${reasons.join("; ")}`;
  if (envelope.cedh_bracket_violation) {
    return { kind: "bracketViolation", message, reasons };
  }
  return { kind: "deckValidation", message, reasons };
}
