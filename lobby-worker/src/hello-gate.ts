// Mirrors phase-server `classify_hello_gate` for the Cloudflare DO shell.

export type HelloGateOutcome =
  | { kind: "accept" }
  | { kind: "reject_handshake" }
  | { kind: "reject_protocol"; client: number; server: number }
  | { kind: "ignore" }
  | { kind: "pass" };

export interface ConnAttachment {
  client_hello: { client_version: string; build_commit: string } | null;
  subscribed: boolean;
  host_game: string | null;
  reservations: unknown[];
  /**
   * Tournament codes this connection CREATED — one appended per successful
   * `CreateTournament`. Mirrors `lobby_broker::ConnState::organized_tournaments`
   * (`crates/lobby-broker/src/broker.rs`), which the broker round-trips through
   * this attachment verbatim.
   *
   * **Never an authority.** Organizer and player permission is the
   * `organizer_token` / `player_token` checked against the stored tournament
   * record — never membership in these lists, which is why nothing in the
   * broker reads them. They are reconnect-convenience bookkeeping for a future
   * "your events" client flow, and nothing in this shell reads them today
   * either; they are declared here so the attachment type stays a faithful
   * mirror of the state that actually crosses the WASM boundary. That warning
   * covers `joined_tournaments` below in equal measure.
   */
  organized_tournaments: string[];
  /**
   * Tournament codes this connection JOINED AS A PLAYER — one appended per
   * successful `JoinTournament`, as distinct from the events it organizes
   * above. Mirrors `lobby_broker::ConnState::joined_tournaments`.
   *
   * **Codes only, never the `player_token`.** The token is the entrant's
   * authority and is deliberately kept out of this list. Membership survives
   * the connection lifecycle, while the broker keeps only a bounded set of
   * recent codes in the attachment. A code carries no such risk, being already
   * broadcast to every subscriber in each `TournamentListUpdate`. The
   * non-authority framing on `organized_tournaments` applies here identically.
   */
  joined_tournaments: string[];
}

/**
 * What this broker accepts at the handshake. Mirrors the `HelloAcceptance::Lobby`
 * arm in `crates/phase-server/src/main.rs`; the Worker is always a LobbyOnly
 * broker, so the full-game arm has no counterpart here.
 */
export interface LobbyHelloPolicy {
  /**
   * The full-game protocol this broker advertises. Used ONLY for the legacy
   * window, against clients built before the lobby owned its own version.
   */
  serverProtocolVersion: number;
  /** The lobby message-set version this broker speaks. */
  lobbyProtocolVersion: number;
  /**
   * Lowest client lobby protocol accepted. There is deliberately no ceiling —
   * a client newer than this broker can only fail by sending a lobby variant
   * the broker does not know, which the Rust core already rejects per-frame as
   * an unknown tag rather than poisoning the connection.
   */
  minSupportedLobbyProtocol: number;
}

export function classifyHelloGate(
  helloReceived: boolean,
  frame: { type?: string; data?: Record<string, unknown> },
  policy: LobbyHelloPolicy,
): HelloGateOutcome {
  if (frame.type === "ClientHello") {
    if (!helloReceived) {
      return classifyClientHello(frame.data, policy);
    }
    return { kind: "ignore" };
  }
  if (!helloReceived) {
    return { kind: "reject_handshake" };
  }
  return { kind: "pass" };
}

function classifyClientHello(
  data: Record<string, unknown> | undefined,
  policy: LobbyHelloPolicy,
): HelloGateOutcome {
  const clientLobbyVersion = data?.lobby_protocol_version;
  // Presence, not truthiness: a client that omits the field is on the legacy
  // path, which is a different policy from one that sent 0.
  if (
    typeof clientLobbyVersion === "number" &&
    !Number.isNaN(clientLobbyVersion)
  ) {
    return clientLobbyVersion < policy.minSupportedLobbyProtocol
      ? {
          kind: "reject_protocol",
          client: clientLobbyVersion,
          server: policy.lobbyProtocolVersion,
        }
      : { kind: "accept" };
  }

  // Legacy: the client predates the lobby-owned version, so all we can gate on
  // is the shared full-game number and its one-version window.
  const legacyMin = Math.max(0, policy.serverProtocolVersion - 1);
  const protocolVersion = Number(data?.protocol_version ?? 0);
  if (
    Number.isNaN(protocolVersion) ||
    protocolVersion < legacyMin ||
    protocolVersion > policy.serverProtocolVersion
  ) {
    return {
      kind: "reject_protocol",
      client: protocolVersion,
      server: policy.serverProtocolVersion,
    };
  }
  return { kind: "accept" };
}

export function helloGateErrorMessage(
  outcome: HelloGateOutcome,
): string | null {
  switch (outcome.kind) {
    case "reject_handshake":
      return "ClientHello required before any other message";
    case "reject_protocol":
      return `Protocol version mismatch: client=${outcome.client} server=${outcome.server}`;
    default:
      return null;
  }
}
