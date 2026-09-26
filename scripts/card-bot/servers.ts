// /lfg availability gate: which builds' lobbies honor bot-chosen room codes, and
// which dedicated servers a bot game may be sent to. Everything comes from the
// build's lobby Worker — its `/health` (the broker's own protocol numbers) and
// `/servers` (the directory, whose rows the Worker attests against each server's
// own `/info`) — so the bot makes no per-server HTTP call.

import { BUILD_ENDPOINTS, BUILDS, type Build } from "./config";

/** lobby-broker LOBBY_PROTOCOL_VERSION entry 10 ("Requested room codes",
 *  crates/lobby-broker/src/protocol.rs): a broker or server at or above this
 *  honors CreateGameWithSettings.requested_code. */
export const REQUESTED_CODE_LOBBY_PROTOCOL = 10;

/** Mirrors `lobby_broker::directory::DIRECTORY_VERSION` (and the client's
 *  serverDirectory.ts constant); pinned by __tests__/servers.test.ts. */
export const DIRECTORY_VERSION = 1;

/** Discord's cap on a string choice value — a server URL is offered as one. */
const MAX_CHOICE_VALUE_LENGTH = 100;

/** Printable ASCII without whitespace. Each such character percent-encodes to at
 *  most three, so the length cap also bounds the web links built from the URL;
 *  a non-ASCII character encodes to up to twelve. */
const PLAIN_URL = /^[\x21-\x7e]+$/;

/** Per-request timeout for the lobby's `/health` and `/servers`. */
const REQUEST_TIMEOUT_MS = 5000;

export interface LobbyInfo {
  protocol_version: number;
  lobby_protocol_version: number;
}

export interface DirectoryServer {
  url: string;
  name: string;
  mode: string;
  protocol_version: number;
  lobby_protocol_version: number;
  current_players: number;
  score: { value: number | null } | null;
}

export interface BuildAvailability {
  broker: LobbyInfo;
  servers: readonly DirectoryServer[];
}

/** The one call shape ServerCache uses. Narrower than `typeof fetch` (which under
 *  @types/bun also carries `preconnect`), so a test stub
 *  `async (url) => new Response(...)` typechecks with no cast; real `fetch` is
 *  assignable to it. */
export type FetchFn = (url: string, init?: RequestInit) => Promise<Response>;

export function brokerSupportsBotGames(broker: LobbyInfo): boolean {
  return broker.lobby_protocol_version >= REQUESTED_CODE_LOBBY_PROTOCOL;
}

/**
 * The dedicated servers a bot game on this build may use: Full servers that honor
 * requested codes and speak exactly the build's client protocol (the broker
 * advertises its build's client PROTOCOL_VERSION). Best score first, then URL, so
 * `[0]` is a deterministic default pick.
 */
export function eligibleServers(
  broker: LobbyInfo,
  rows: readonly DirectoryServer[],
): DirectoryServer[] {
  return rows
    .filter(
      (row) =>
        row.mode === "Full" &&
        row.lobby_protocol_version >= REQUESTED_CODE_LOBBY_PROTOCOL &&
        row.protocol_version === broker.protocol_version &&
        row.url.length <= MAX_CHOICE_VALUE_LENGTH &&
        PLAIN_URL.test(row.url),
    )
    .sort(
      (a, b) =>
        (b.score?.value ?? -1) - (a.score?.value ?? -1) ||
        (a.url < b.url ? -1 : a.url > b.url ? 1 : 0),
    );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isLobbyInfo(body: unknown): body is LobbyInfo {
  return (
    isRecord(body) &&
    isFiniteNumber(body.protocol_version) &&
    isFiniteNumber(body.lobby_protocol_version)
  );
}

function isDirectoryServer(row: unknown): row is DirectoryServer {
  if (!isRecord(row)) return false;
  const { score } = row;
  return (
    typeof row.url === "string" &&
    typeof row.name === "string" &&
    typeof row.mode === "string" &&
    isFiniteNumber(row.protocol_version) &&
    isFiniteNumber(row.lobby_protocol_version) &&
    isFiniteNumber(row.current_players) &&
    (score === null || (isRecord(score) && (score.value === null || isFiniteNumber(score.value))))
  );
}

/**
 * Reads a `GET /servers` body (untrusted). Same rule as the client's
 * `projectDirectoryBody`: a shape this bot cannot read is not partially readable,
 * so a non-object body, a `directory_version` other than DIRECTORY_VERSION, or a
 * non-array `servers` yields `[]`; otherwise only well-formed rows are kept.
 */
export function parseDirectory(body: unknown): DirectoryServer[] {
  if (
    !isRecord(body) ||
    body.directory_version !== DIRECTORY_VERSION ||
    !Array.isArray(body.servers)
  ) {
    return [];
  }
  return body.servers.filter(isDirectoryServer);
}

export class ServerCache {
  private readonly entries = new Map<Build, BuildAvailability | null>();

  constructor(private readonly fetchFn: FetchFn = fetch) {}

  /** The last refresh's result for a build; `null` when its lobby is unknown
   *  (not yet refreshed, or its `/health` failed). */
  get(build: Build): BuildAvailability | null {
    return this.entries.get(build) ?? null;
  }

  /** Re-reads every build. Builds are independent; only a failed `/health`
   *  nulls a build — a failed `/servers` leaves P2P available with no servers. */
  async refresh(): Promise<void> {
    await Promise.allSettled(
      BUILDS.map(async (build) => {
        const { lobbyHttp } = BUILD_ENDPOINTS[build];
        try {
          const [broker, servers] = await Promise.all([
            this.loadBroker(lobbyHttp),
            this.loadDirectory(lobbyHttp),
          ]);
          this.entries.set(build, { broker, servers });
        } catch (err) {
          this.entries.set(build, null);
          console.error(`[servers] ${build} lobby health check failed:`, err);
        }
      }),
    );
  }

  /** Refreshes now, then every `intervalMs`. */
  start(intervalMs = 30_000): void {
    void this.refresh();
    setInterval(() => void this.refresh(), intervalMs);
  }

  /** The broker's own protocol numbers. Rejects on any failure. */
  private async loadBroker(lobbyHttp: string): Promise<LobbyInfo> {
    const res = await this.fetchFn(`${lobbyHttp}/health`, {
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
    if (!res.ok) throw new Error(`/health → ${res.status}`);
    const body: unknown = await res.json();
    if (!isLobbyInfo(body)) throw new Error("/health: unexpected body");
    return {
      protocol_version: body.protocol_version,
      lobby_protocol_version: body.lobby_protocol_version,
    };
  }

  /** The directory's rows. Never rejects: a timeout, network error, non-2xx
   *  status or unreadable body is `[]`, so it cannot discard a good `/health`. */
  private async loadDirectory(lobbyHttp: string): Promise<DirectoryServer[]> {
    try {
      const res = await this.fetchFn(`${lobbyHttp}/servers`, {
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      });
      if (!res.ok) return [];
      return parseDirectory(await res.json());
    } catch (err) {
      console.error(`[servers] ${lobbyHttp}/servers failed:`, err);
      return [];
    }
  }
}
