import {
  acknowledgeFullTerminalDelivery,
  bootstrapFullTerminalDelivery,
  readFullTerminalResult,
} from "../adapter/ws-adapter";
import {
  commitFullTerminalDelivery,
  loadFullTerminalDelivery,
  replaceFullTerminalDelivery,
  type FullTerminalDelivery,
} from "./fullTerminalResult";
import type { FullSessionKey } from "./multiplayerSession";
import { sameFullSessionKey } from "./fullSessionKey";
import type { PhaseSocketFactory } from "./openPhaseSocket";

/** A persisted recovery pointer for one Full seat. */
export interface FullTerminalRoute {
  serverUrl: string;
  fullKey: FullSessionKey;
  /** Pre-terminal player token: the terminal bootstrap's proof of seat. */
  playerToken: string;
  /** The pointer already committed a delivery and must never reconnect to gameplay. */
  terminalOnly: boolean;
  /** Raw transport for the terminal-only requests; absent for browser WebSockets. */
  socketFactory?: PhaseSocketFactory;
}

export type FullTerminalRecoveryEvent =
  | { type: "terminalDelivery"; delivery: FullTerminalDelivery }
  | { type: "terminalUnavailable"; message: string };

/** The pointer's owner: it persists the route and publishes the result. */
export interface FullTerminalRecoveryOwner {
  isCancelled(): boolean;
  /** Publishes a recovery event. Never called once cancelled. */
  emit(event: FullTerminalRecoveryEvent): void;
  /** Runs before a server or storage failure is published. */
  onFailure?(): void;
  /** Message for a terminal-only pointer whose committed delivery cannot be read. */
  unreadableMessage: string;
  /** Marks the stored pointer terminal-only, leaving any replacement pointer alone. */
  retainTerminalRoute(): void;
  /** Removes the stored pointer after the server acknowledged, leaving any replacement alone. */
  releaseTerminalRoute(): void;
}

/** `reconnect`: the server holds no terminal result, so the seat resumes play. */
export type FullTerminalRecoveryOutcome = "reconnect" | "settled";

/**
 * Recovers a seat's terminal result before any playable reconnect. A delivery
 * committed locally is re-displayed and re-acknowledged; otherwise the server
 * is asked through the terminal-only bootstrap, because a game whose result
 * was prepared has retired the key that an ordinary reconnect presents. Only a
 * seat the server reports as still live returns `reconnect`.
 *
 * Every step that holds a result keeps the pointer terminal-only until the
 * server acknowledges the delivery, so a failed ACK, a lost socket or a closed
 * app recovers the same result on the next open.
 */
export async function recoverFullTerminal(
  route: FullTerminalRoute,
  owner: FullTerminalRecoveryOwner,
): Promise<FullTerminalRecoveryOutcome> {
  const publish = (event: FullTerminalRecoveryEvent) => {
    if (!owner.isCancelled()) owner.emit(event);
  };
  const fail = (error: unknown, fallback: string) => {
    if (owner.isCancelled()) return;
    owner.onFailure?.();
    owner.emit({ type: "terminalUnavailable", message: error instanceof Error ? error.message : fallback });
  };
  const acknowledge = (delivery: FullTerminalDelivery) => {
    void acknowledgeFullTerminalDelivery(
      route.serverUrl,
      delivery.deliveryId,
      delivery.credential,
      route.socketFactory,
    ).then((acknowledged) => {
      if (acknowledged) owner.releaseTerminalRoute();
    }).catch(() => {});
  };

  const retained = await loadFullTerminalDelivery(route.fullKey);
  if (owner.isCancelled()) return "settled";
  if (route.terminalOnly && !retained) {
    publish({ type: "terminalUnavailable", message: owner.unreadableMessage });
    return "settled";
  }

  if (retained) {
    // A retained terminal capability is read only through the raw terminal
    // socket helpers: no playable adapter, controller, deck, or reconnect.
    owner.retainTerminalRoute();
    // Show the cached result summary immediately. A refreshed delivery must
    // establish matching authority before reusing a private view.
    publish({ type: "terminalDelivery", delivery: { ...retained, finalView: undefined } });
    try {
      // The read is bound to the retained credential by the request helper.
      // A reply naming another game or delivery is still not this pointer's
      // result: it is never retained, published, or acknowledged, and the
      // retained summary stays on screen with the pointer intact.
      const refreshed = await readFullTerminalResult(route.serverUrl, retained.credential, route.socketFactory);
      if (owner.isCancelled()) return "settled";
      if (refreshed !== null && !sameTerminalAuthority(refreshed, retained)) {
        throw new Error("Mismatched terminal delivery read");
      }
      const effectiveRefresh = refreshed !== null
        && refreshed.finalView === undefined
        && retained.finalView !== undefined
        ? { ...refreshed, finalView: retained.finalView }
        : refreshed;
      if (effectiveRefresh && !(await replaceFullTerminalDelivery(effectiveRefresh))) {
        throw new Error("Failed to retain terminal delivery");
      }
      const display = effectiveRefresh ?? retained;
      acknowledge(display);
      publish({ type: "terminalDelivery", delivery: display });
    } catch (error) {
      fail(error, "Terminal result is unavailable");
    }
    return "settled";
  }

  // The bootstrap reply is bound to `route.fullKey` by the request helper: a
  // delivery for another game or generation throws before it is committed.
  let bootstrap: FullTerminalDelivery | null;
  try {
    bootstrap = await bootstrapFullTerminalDelivery(
      route.serverUrl,
      route.fullKey,
      route.playerToken,
      crypto.randomUUID(),
      route.socketFactory,
    );
  } catch (error) {
    fail(error, "Terminal result is unavailable");
    return "settled";
  }
  if (owner.isCancelled()) return "settled";
  if (!bootstrap) return "reconnect";
  if (!(await commitFullTerminalDelivery(bootstrap))) {
    fail(null, "Failed to retain terminal delivery");
    return "settled";
  }
  owner.retainTerminalRoute();
  acknowledge(bootstrap);
  publish({ type: "terminalDelivery", delivery: bootstrap });
  return "settled";
}

/** Whether two deliveries carry the same terminal authority for one seat. */
function sameTerminalAuthority(a: FullTerminalDelivery, b: FullTerminalDelivery): boolean {
  return sameFullSessionKey(a.key, b.key)
    && a.deliveryId === b.deliveryId
    && a.credential === b.credential;
}
