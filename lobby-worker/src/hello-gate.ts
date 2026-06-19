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
}

export function classifyHelloGate(
  helloReceived: boolean,
  frame: { type?: string; data?: Record<string, unknown> },
  serverProtocolVersion: number,
): HelloGateOutcome {
  const minSupportedProtocol = Math.max(0, serverProtocolVersion - 1);
  if (frame.type === "ClientHello") {
    if (!helloReceived) {
      const protocolVersion = Number(frame.data?.protocol_version ?? 0);
      if (
        Number.isNaN(protocolVersion) ||
        protocolVersion < minSupportedProtocol ||
        protocolVersion > serverProtocolVersion
      ) {
        return {
          kind: "reject_protocol",
          client: protocolVersion,
          server: serverProtocolVersion,
        };
      }
      return { kind: "accept" };
    }
    return { kind: "ignore" };
  }
  if (!helloReceived) {
    return { kind: "reject_handshake" };
  }
  return { kind: "pass" };
}

export function helloGateErrorMessage(outcome: HelloGateOutcome): string | null {
  switch (outcome.kind) {
    case "reject_handshake":
      return "ClientHello required before any other message";
    case "reject_protocol":
      return `Protocol version mismatch: client=${outcome.client} server=${outcome.server}`;
    default:
      return null;
  }
}
