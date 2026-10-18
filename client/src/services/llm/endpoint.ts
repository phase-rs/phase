import { useLlmStore } from "../../stores/llmStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { defaultJevRelayOrigin } from "./relayOrigin";
import { endpointOf, type LlmEndpointConfig, type LlmProfile } from "./types";

/**
 * The endpoint the engine is handed for a profile.
 *
 * Jev is not called directly: TypeSafe's API refuses browser CORS, so its
 * requests go through a phase-server relay. A Jev profile with no endpoint of
 * its own uses the multiplayer server the player chose (or this build's
 * default), which is known here and not to the engine. Every other provider is passed through as
 * saved; the engine decides what each endpoint means and refuses one it cannot
 * use.
 *
 * A derived relay is resolved against ONE matching snapshot of the profile.
 * Callers (a draft round, a probe) hold their copy across awaits, and the store
 * both drops a key when its relay origin changes (`llmStore`) and lets the
 * player retarget the profile and enter a replacement key meanwhile. Mixing the
 * held copy's provider and blank endpoint (which derive the OLD relay) with the
 * current profile's key (entered for a NEW endpoint) would deliver one server's
 * key to another. So the current key is used only when the current profile is
 * the same endpoint as the held copy — same provider, same raw endpoint — and
 * otherwise the request goes out with no key, which the engine refuses to
 * build. A derived-relay profile the store no longer holds has no key either.
 * (A held profile with an explicit endpoint keeps sending its own key to its own
 * endpoint, which is where that key was entered for.)
 */
export function resolvedEndpointOf(profile: LlmProfile): LlmEndpointConfig {
  const endpoint = endpointOf(profile);
  if (endpoint.provider !== "Jev" || endpoint.baseUrl?.trim()) return endpoint;
  const current = useLlmStore.getState().profiles.find((candidate) => candidate.id === profile.id);
  const sameEndpoint =
    current !== undefined
    && current.provider === profile.provider
    && (current.baseUrl ?? "").trim() === (profile.baseUrl ?? "").trim();
  return {
    ...endpoint,
    apiKey: sameEndpoint ? current.apiKey : "",
    baseUrl: defaultJevRelayOrigin(useMultiplayerStore.getState().hostingServer) ?? null,
  };
}
