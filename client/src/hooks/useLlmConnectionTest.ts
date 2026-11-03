import { useCallback, useEffect, useRef } from "react";

import { testLlmEndpoint } from "../services/llm/probe";
import type { LlmFailure, LlmProfile } from "../services/llm/types";

/** What the settings UI shows for a profile's connection test. */
export type LlmTestState =
  | { status: "idle" }
  | { status: "running" }
  | { status: "ok" }
  | ({ status: "failed" } & LlmFailure);

/**
 * Test a profile end to end.
 *
 * Delegates to {@link testLlmEndpoint}, which builds the request with the
 * engine, performs it, and validates the reply with the engine's own decoder.
 * A resolved fetch is NOT success: the transport returns non-2xx bodies so a
 * vendor's message survives to be shown, and both of the failures a player is
 * most likely to hit — a rejected key and an unknown model — arrive as
 * well-formed HTTP responses.
 *
 * Needs no game: the probe is stateless, which is what makes it usable at the
 * moment a player is actually configuring a provider.
 */
export function useLlmConnectionTest(
  profile: LlmProfile,
  setTest: (state: LlmTestState) => void,
): () => void {
  // One generation per probe. A result is published only if its generation is
  // still the current one, so a probe for an endpoint the player has since
  // edited cannot paint "Connected" over the new configuration.
  const generation = useRef(0);
  const inFlight = useRef<AbortController | null>(null);

  const abandon = useCallback(() => {
    generation.current += 1;
    inFlight.current?.abort();
    inFlight.current = null;
  }, []);

  // Any change to what is being tested invalidates a probe already running, and
  // unmounting abandons it outright.
  useEffect(() => {
    abandon();
    setTest({ status: "idle" });
  }, [profile.provider, profile.baseUrl, profile.model, profile.apiKey, abandon, setTest]);
  useEffect(() => abandon, [abandon]);

  return useCallback(() => {
    abandon();
    const ticket = generation.current;
    const controller = new AbortController();
    inFlight.current = controller;
    void (async () => {
      setTest({ status: "running" });
      const result = await testLlmEndpoint(profile, { signal: controller.signal });
      // The endpoint moved on while this was in flight: its verdict describes a
      // configuration that is no longer on screen, so it is discarded.
      if (ticket !== generation.current) return;
      inFlight.current = null;
      setTest(
        result.ok
          ? { status: "ok" }
          : { status: "failed", code: result.code, detail: result.detail },
      );
    })();
  }, [profile, setTest, abandon]);
}
