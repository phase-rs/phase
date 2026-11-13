import type { FormatConfig } from "../adapter/types";

/**
 * Draft-match launch authority (CR 100.2b + CR 702.139b). A Cube may configure
 * a minimum deck size other than 40, and the engine reads `deck_size` for every
 * "minimum deck size" rule, notably Yorion's companion condition.
 *
 * The minimum is NEVER a field of the format payload and never a plain
 * parameter an ordinary launch can pass. `limitedFormatConfigForSession` (called
 * only by the accepted draft's own match launch, with that draft's resolved
 * `min_deck_size`) registers the minimum against the exact config object it
 * returns; the adapter reads it back with `draftSessionAuthorityOf` and starts
 * the game through the engine's distinct draft-match initializers. A copy,
 * spread or hand-edited config is a different object and carries no authority,
 * so it launches (and is refused) as an ordinary game.
 */
const sessionMinimums = new WeakMap<FormatConfig, number>();

export function limitedFormatConfigForSession(
  base: FormatConfig,
  sessionMinDeckSize: number | null | undefined,
): FormatConfig {
  if (sessionMinDeckSize == null || base.deck_size.type !== "Minimum") return base;
  if (base.deck_size.data === sessionMinDeckSize) return base;
  const config: FormatConfig = { ...base };
  sessionMinimums.set(config, sessionMinDeckSize);
  return config;
}

/** The draft session's minimum this exact launch config was issued with, if any. */
export function draftSessionAuthorityOf(config: FormatConfig | null | undefined): number | undefined {
  return config ? sessionMinimums.get(config) : undefined;
}

/**
 * The minimum deck size a launch config carries, for the engine's compatibility
 * request: the draft session's when issued, else the format's own `Minimum`.
 * `undefined` for a fixed-size rule, which has no minimum to override.
 */
export function minimumDeckSizeOf(config: FormatConfig | undefined): number | undefined {
  const session = draftSessionAuthorityOf(config);
  if (session !== undefined) return session;
  return config?.deck_size.type === "Minimum" ? config.deck_size.data : undefined;
}
