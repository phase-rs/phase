import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../stores/gameStore";
import {
  isMultiplayerDraftPodLive,
  type MultiplayerDraftPhase,
  useMultiplayerDraftStore,
} from "../../stores/multiplayerDraftStore";
import {
  deferUntilMultiplayerSessionEnds,
  isMultiplayerGameLive,
  whenMultiplayerGameEnds,
} from "../multiplayerGuard";

const liveDraftPhases: MultiplayerDraftPhase[] = [
  "connecting",
  "lobby",
  "drafting",
  "deckbuilding",
  "pairing",
  "matchInProgress",
  "roundComplete",
];

function setRemoteGameLive(live: boolean): void {
  useGameStore.setState({
    gameMode: live ? "p2p-host" : null,
    adapter: live ? ({} as never) : null,
    gameState: null,
  });
}

function setDraftPod(role: "host" | "guest" | null, phase: MultiplayerDraftPhase): void {
  useMultiplayerDraftStore.setState({ role, phase });
}

describe("multiplayerGuard", () => {
  beforeEach(() => {
    setRemoteGameLive(false);
    setDraftPod(null, "idle");
  });

  afterEach(() => {
    setRemoteGameLive(false);
    setDraftPod(null, "idle");
  });

  it.each(liveDraftPhases)("keeps update actions parked during draft pod %s", (phase) => {
    setDraftPod("guest", phase);

    expect(isMultiplayerGameLive()).toBe(true);
    expect(isMultiplayerDraftPodLive(useMultiplayerDraftStore.getState())).toBe(true);
  });

  it.each<MultiplayerDraftPhase>(["idle", "complete", "error", "kicked", "hostLeft"])(
    "does not treat terminal draft pod %s as live",
    (phase) => {
      setDraftPod("host", phase);

      expect(isMultiplayerGameLive()).toBe(false);
    },
  );

  it("requires a pod role so stale phases do not block an update", () => {
    setDraftPod(null, "deckbuilding");

    expect(isMultiplayerGameLive()).toBe(false);
  });

  it("waits for both a remote game and a draft pod to end", () => {
    setRemoteGameLive(true);
    setDraftPod("host", "deckbuilding");
    const callback = vi.fn();

    const cancel = whenMultiplayerGameEnds(callback);
    setRemoteGameLive(false);

    expect(callback).not.toHaveBeenCalled();

    setDraftPod("host", "complete");

    expect(callback).toHaveBeenCalledTimes(1);
    cancel();
  });

  it("runs a deferred action exactly once and clears its cancellation handle", () => {
    setDraftPod("guest", "pairing");
    const action = vi.fn();

    const pending = deferUntilMultiplayerSessionEnds(action);

    expect(pending.deferred).toBe(true);
    setDraftPod("guest", "complete");
    setDraftPod("guest", "hostLeft");
    pending.cancel();

    expect(action).toHaveBeenCalledTimes(1);
  });

  it("runs an activation before queued reloads when the pod ends", () => {
    setDraftPod("host", "deckbuilding");
    const actions: string[] = [];

    deferUntilMultiplayerSessionEnds(() => actions.push("reload-a"), "reload");
    deferUntilMultiplayerSessionEnds(() => actions.push("activation"), "activation");
    deferUntilMultiplayerSessionEnds(() => actions.push("reload-b"), "reload");
    deferUntilMultiplayerSessionEnds(() => actions.push("install"), "install");
    setDraftPod("host", "complete");

    expect(actions).toEqual(["activation", "reload-a", "reload-b", "install"]);
  });

  it("cancels only its own queued action while other callers remain parked", () => {
    setDraftPod("host", "drafting");
    const cancelled = vi.fn();
    const retained = vi.fn();

    const first = deferUntilMultiplayerSessionEnds(cancelled, "reload");
    deferUntilMultiplayerSessionEnds(retained, "activation");
    first.cancel();
    setDraftPod("host", "complete");

    expect(cancelled).not.toHaveBeenCalled();
    expect(retained).toHaveBeenCalledTimes(1);
  });

  it("cancels a deferred action without retaining an end callback", () => {
    setRemoteGameLive(true);
    const action = vi.fn();

    const pending = deferUntilMultiplayerSessionEnds(action);
    pending.cancel();
    setRemoteGameLive(false);

    expect(action).not.toHaveBeenCalled();
  });

  it("runs an action immediately when neither session is live", () => {
    const action = vi.fn();

    const pending = deferUntilMultiplayerSessionEnds(action);

    expect(pending.deferred).toBe(false);
    expect(action).toHaveBeenCalledTimes(1);
  });
});
