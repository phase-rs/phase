/**
 * `key` from the deployment's `/config.js`, or `null` when it is absent, not a
 * non-empty string, or refused by `accept`.
 *
 * That file is untrusted (see `PhaseRuntimeConfig`), so every key is read here
 * and each caller falls back to its build-time define on `null`.
 */
export function runtimeConfigValue(
  key: keyof PhaseRuntimeConfig,
  accept: (value: string) => boolean,
): string | null {
  if (typeof window === "undefined") return null;
  const configured: unknown = window.__PHASE_CONFIG__?.[key];
  if (typeof configured !== "string" || configured === "") return null;
  return accept(configured) ? configured : null;
}
