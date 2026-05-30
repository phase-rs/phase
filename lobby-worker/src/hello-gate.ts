// Mirrors phase-server `classify_hello_gate` for the Cloudflare DO shell.

import { PROTOCOL_VERSION } from "./protocol";

export const MIN_SUPPORTED_PROTOCOL = PROTOCOL_VERSION - 1;

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
): HelloGateOutcome {
  if (frame.type === "ClientHello") {
    if (!helloReceived) {
      const protocolVersion = Number(frame.data?.protocol_version ?? 0);
      if (
        protocolVersion < MIN_SUPPORTED_PROTOCOL ||
        protocolVersion > PROTOCOL_VERSION
      ) {
        return {
          kind: "reject_protocol",
          client: protocolVersion,
          server: PROTOCOL_VERSION,
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
