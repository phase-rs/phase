/**
 * The official lobby broker for THIS build's release channel.
 *
 * Channel-scoped rather than a single fixed address. The lobby advertises its
 * versions blind (it sends `ServerHello` before the client's `ClientHello`), and
 * clients built before `lobby_protocol_version` existed accept a lobby only
 * within `[PROTOCOL_VERSION - 1, PROTOCOL_VERSION]` of their OWN build.
 * Production's Worker redeploys only at release while preview rebuilds from
 * `main`, so once `main` is two protocol bumps past the last tag those windows
 * are disjoint and a single shared lobby must lock one channel out.
 *
 * `LOBBY_PROTOCOL_VERSION` (see `ws-adapter.ts`) removes that coupling for
 * current builds, but already-deployed clients still gate on the shared number —
 * so each channel keeps its own broker. `deploy.yml` sets this to the preview
 * lobby.
 *
 * Defaults to the production lobby, so release and self-hosted builds are
 * unchanged.
 */
export const OFFICIAL_MULTIPLAYER_SERVER_URL = __OFFICIAL_MULTIPLAYER_SERVER_URL__;

/**
 * Parse a `ws://`/`wss://` URL, or `null` if it is not one.
 *
 * Defined here rather than in `serverDetection` because this module is a leaf
 * and has to validate the runtime override below without importing one of its
 * own consumers. `serverDetection` re-exports it, so there is still exactly one
 * implementation and every existing import site is unchanged.
 */
export function parseWebSocketUrl(value: string): URL | null {
  try {
    const url = new URL(value);
    if ((url.protocol !== "ws:" && url.protocol !== "wss:") || !url.host) {
      return null;
    }
    // `new WebSocket()` throws a SyntaxError on any fragment, so a URL carrying
    // one is not a valid address however well-formed it looks. Tested on `href`
    // rather than `hash`, which is "" for a bare trailing "#" that still throws;
    // in the serialized form a literal "#" can only be the fragment delimiter,
    // since a "#" anywhere else percent-encodes to %23.
    if (url.href.includes("#")) {
      return null;
    }
    return url;
  } catch {
    return null;
  }
}

/**
 * The default server a deployment configured at runtime, or `null`.
 *
 * A self-hosted deployment serves its own `/config.js` (see
 * `client/public/config.js`), which lets one prebuilt bundle be pointed at any
 * server without a rebuild — the helm chart renders it from
 * `web.defaultMultiplayerServerUrl`. A malformed value is ignored rather than
 * propagated: a typo'd address would otherwise become the seed for every new
 * profile, with nothing to tell the player why nothing connects.
 */
function runtimeDefaultServerUrl(): string | null {
  if (typeof window === "undefined") return null;
  const configured = window.__PHASE_CONFIG__?.multiplayerServerUrl;
  if (typeof configured !== "string" || configured === "") return null;
  return parseWebSocketUrl(configured) ? configured : null;
}

/**
 * Where this build connects by default.
 *
 * Runtime configuration wins over the build-time define so one generic image
 * can be deployed anywhere; with no `/config.js` override the define is used
 * unchanged, which is what every official build does.
 *
 * Read once at module load: `serverDetection` derives `SERVER_PRESETS` from
 * this at import time, so re-reading later would let the picker and the store
 * disagree about what "default" means.
 */
export const DEFAULT_MULTIPLAYER_SERVER_URL =
  runtimeDefaultServerUrl() ?? __DEFAULT_MULTIPLAYER_SERVER_URL__;

/** Hosts we operate. Every channel's broker belongs here — `isOfficial…` gates
 * the persisted-address migration, which treats an address on any of these as a
 * deployment default rather than user intent. */
const OFFICIAL_MULTIPLAYER_SERVER_HOSTS = new Set([
  "lobby.phase-rs.dev",
  "lobby-preview.phase-rs.dev",
  "us.phase-rs.dev",
]);

export function isOfficialMultiplayerServerUrl(value: string): boolean {
  try {
    return OFFICIAL_MULTIPLAYER_SERVER_HOSTS.has(new URL(value).hostname);
  } catch {
    return false;
  }
}
