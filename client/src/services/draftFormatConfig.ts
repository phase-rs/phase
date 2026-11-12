import type { FormatConfig } from "../adapter/types";

/**
 * The Limited rules a draft match runs under: the format's own config with the
 * draft SESSION's resolved minimum deck size (CR 100.2b). A Cube may configure
 * a minimum other than 40, and the engine reads `deck_size` for every
 * "minimum deck size" rule, notably Yorion's companion condition
 * (CR 702.139b), so the launch must hand it over rather than the registry
 * default. The override is tagged `deck_size_origin: "DraftSession"`, the
 * provenance the engine's `FormatConfig` decoder requires before it accepts a
 * Limited minimum other than the registry's 40. `undefined` (no session
 * minimum known) keeps the base config.
 */
export function limitedFormatConfigForSession(
  base: FormatConfig,
  sessionMinDeckSize: number | null | undefined,
): FormatConfig {
  if (sessionMinDeckSize == null || base.deck_size.type !== "Minimum") return base;
  if (base.deck_size.data === sessionMinDeckSize) return base;
  return {
    ...base,
    deck_size: { type: "Minimum", data: sessionMinDeckSize },
    deck_size_origin: "DraftSession",
  };
}

/**
 * The minimum deck size a resolved format config carries, for the engine's
 * compatibility request. `undefined` for a fixed-size rule, which has no
 * minimum to override.
 */
export function minimumDeckSizeOf(config: FormatConfig | undefined): number | undefined {
  return config?.deck_size.type === "Minimum" ? config.deck_size.data : undefined;
}
