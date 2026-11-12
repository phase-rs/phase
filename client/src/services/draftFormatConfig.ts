import type { FormatConfig } from "../adapter/types";

/**
 * The Limited rules a draft match runs under: the format's own config plus the
 * draft SESSION's resolved minimum deck size (CR 100.2b). A Cube may configure
 * a minimum other than 40, and the engine reads `deck_size` for every
 * "minimum deck size" rule, notably Yorion's companion condition
 * (CR 702.139b).
 *
 * The minimum travels in `draft_session_min_deck_size`, a client-side carrier
 * that is NEVER part of the format payload: `splitDraftSessionAuthority`
 * lifts it into the engine's draft-match initializer parameter, which is the
 * only place the engine confers a non-registry Limited deck size. The format
 * payload itself keeps the registry's `deck_size`, so a generic payload cannot
 * grant itself the exception. `undefined` (no session minimum known) keeps
 * the base config.
 */
export function limitedFormatConfigForSession(
  base: FormatConfig,
  sessionMinDeckSize: number | null | undefined,
): FormatConfig {
  if (sessionMinDeckSize == null || base.deck_size.type !== "Minimum") return base;
  if (base.deck_size.data === sessionMinDeckSize) return base;
  return { ...base, draft_session_min_deck_size: sessionMinDeckSize };
}

/**
 * Splits a launch config at the engine boundary: the wire `FormatConfig`
 * (carrier removed) and the draft session's minimum for the engine's
 * draft-match initializer parameter (`undefined` for every non-draft launch).
 */
export function splitDraftSessionAuthority(config: FormatConfig | null | undefined): {
  formatConfig: FormatConfig | null;
  draftSessionMinDeckSize: number | undefined;
} {
  if (!config) return { formatConfig: null, draftSessionMinDeckSize: undefined };
  const { draft_session_min_deck_size: draftSessionMinDeckSize, ...formatConfig } = config;
  return { formatConfig, draftSessionMinDeckSize };
}

/**
 * The minimum deck size a resolved launch config carries, for the engine's
 * compatibility request: the draft session's when present, else the format's
 * own `Minimum`. `undefined` for a fixed-size rule, which has no minimum to
 * override.
 */
export function minimumDeckSizeOf(config: FormatConfig | undefined): number | undefined {
  if (config?.draft_session_min_deck_size != null) return config.draft_session_min_deck_size;
  return config?.deck_size.type === "Minimum" ? config.deck_size.data : undefined;
}
