import { DEFAULT_MULTIPLAYER_SERVER_URL, serverHttpOrigin } from "../../config/multiplayerServer";

/**
 * The HTTP origin a Jev profile with no endpoint of its own relays through: the
 * multiplayer server the player is hosting on, or this build's default when
 * there is none. `undefined` when no origin can be derived.
 *
 * This is the one place that derivation lives. The credential binding in
 * `llmStore` compares successive results to know when a key's destination moved,
 * and `resolvedEndpointOf` uses the same result as the endpoint it hands the
 * engine, so what a key was entered for and where it is sent cannot disagree.
 */
export function defaultJevRelayOrigin(hostingServer: string | null): string | undefined {
  return serverHttpOrigin(hostingServer ?? DEFAULT_MULTIPLAYER_SERVER_URL);
}
