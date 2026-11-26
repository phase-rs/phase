import "fake-indexeddb/auto";
import { clear as idbClear } from "idb-keyval";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";

/*
 * Admitting a native setup, and the pod store's create and join under one, on
 * the real pod store, lifecycle store and storage (fake-indexeddb). The
 * doubles, each stated here:
 * - `ServerDraftAdapter`: the constructor records its URL; `createDraft` and
 *   `joinDraft` record their arguments and their settlers in `settles`, and
 *   settle only when a test settles them; `onEvent` returns a no-op removal;
 *   `credential` is `null`; `dispose` counts a call.
 * - `DraftAdapter.draftProcedure`: resolves `draftProcedureFixture()`.
 * - `network/connection`'s `hostRoom` and `joinRoom`: never settle.
 * - `fetch`: never settles.
 */
type NativeSettle = { resolve: (value?: unknown) => void; reject: (error: Error) => void };
const doubles = vi.hoisted(() => {
  const instances: Array<{
    readonly url: string;
    readonly createCalls: unknown[];
    readonly joinCalls: unknown[][];
    readonly settles: NativeSettle[];
    disposeCount: number;
  }> = [];
  class NativeDouble {
    readonly createCalls: unknown[] = [];
    readonly joinCalls: unknown[][] = [];
    readonly settles: NativeSettle[] = [];
    disposeCount = 0;
    readonly credential = null;
    constructor(readonly url: string) {
      instances.push(this);
    }
    onEvent(): () => void {
      return () => {};
    }
    createDraft(settings: unknown): Promise<void> {
      this.createCalls.push(settings);
      return new Promise((resolve, reject) => {
        this.settles.push({ resolve: () => resolve(), reject });
      });
    }
    joinDraft(...args: unknown[]): Promise<unknown> {
      this.joinCalls.push(args);
      return new Promise((resolve, reject) => {
        this.settles.push({ resolve, reject });
      });
    }
    dispose(): void {
      this.disposeCount += 1;
    }
  }
  return {
    instances,
    NativeDouble,
    draftProcedure: vi.fn(),
    hostRoom: vi.fn(() => new Promise(() => {})),
    joinRoom: vi.fn(() => new Promise(() => {})),
  };
});

vi.mock("../../adapter/server-draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/server-draft-adapter")>()),
  ServerDraftAdapter: doubles.NativeDouble,
}));

vi.mock("../../adapter/draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter")>()),
  DraftAdapter: class {
    draftProcedure = doubles.draftProcedure;
  },
}));

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  hostRoom: doubles.hostRoom,
  joinRoom: doubles.joinRoom,
}));

import { useDraftPodStore, type NativeDraftSetup } from "../draftPodStore";
import { DRAFT_OFFLINE_ERROR, useMultiplayerDraftStore } from "../multiplayerDraftStore";
import { useConnectivityStore } from "../connectivityStore";
import { canonicalNativeDraftEndpoint, getDraftStore } from "../../services/draftPersistence";

const pod = () => useDraftPodStore.getState();
const fetchDouble = vi.fn(() => new Promise<Response>(() => {}));

/** Admits `endpoint` and answers the setup the store now holds. */
function admit(endpoint: string): NativeDraftSetup {
  const admission = pod().admitNativeDraftSetup(endpoint);
  expect(admission.type).toBe("admitted");
  const setup = pod().nativeSetup;
  expect(setup).not.toBeNull();
  return setup!;
}

/** A promise the test settles. */
function held<T = void>(): { promise: Promise<T>; resolve: (value: T) => void; reject: (error: Error) => void } {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** Lets every pending continuation run. */
async function flush(): Promise<void> {
  for (let turn = 0; turn < 5; turn++) await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("admitting a native draft setup", () => {
  beforeEach(async () => {
    vi.clearAllMocks();
    doubles.instances.length = 0;
    doubles.draftProcedure.mockReset().mockResolvedValue(draftProcedureFixture());
    vi.stubGlobal("fetch", fetchDouble);
    localStorage.clear();
    await idbClear(getDraftStore());
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    await useMultiplayerDraftStore.getState().leave(true);
    useMultiplayerDraftStore.getState().reset();
    pod().reset();
  });

  afterEach(async () => {
    await useMultiplayerDraftStore.getState().leave(true);
    pod().reset();
    vi.unstubAllGlobals();
  });

  it.each([
    "ws://h:1/ws?x=1",
    "ws://h:1/ws?",
    "ws://u:p@h:1/ws",
    "ws://h:1/ws#f",
    "https://h/ws",
    "",
  ])("refuses %j, writing nothing", (endpoint) => {
    const first = admit("ws://h:9374/ws");
    // Reach: an admitted setup is held before the refused one.
    expect(pod().nativeSetup).toBe(first);

    expect(pod().admitNativeDraftSetup(endpoint)).toEqual({ type: "refused" });
    expect(pod().nativeSetup).toBe(first);

    // A refusal did not advance the generation.
    expect(admit("ws://h:9374/ws").generation).toBe(first.generation + 1);
  });

  it("stores the canonical endpoint and answers the route to open", () => {
    const admission = pod().admitNativeDraftSetup(" WS://Host:9374/ws ");
    const setup = pod().nativeSetup;
    // Reach: admitted.
    expect(setup).not.toBeNull();

    expect(setup!.endpoint).toBe("ws://host:9374/ws");
    expect(admission).toEqual({
      type: "admitted",
      to: "/draft-pod?entry=native",
      state: { nativeDraftSetup: setup!.generation },
    });
    expect(canonicalNativeDraftEndpoint(setup!.endpoint)).toBe(setup!.endpoint);
  });

  it("is cleared by a reset", () => {
    admit("ws://h:9374/ws");
    // Reach: admitted.
    expect(pod().nativeSetup).not.toBeNull();

    pod().reset();
    expect(pod().nativeSetup).toBeNull();
  });

  describe("a setup the store no longer holds", () => {
    const seedForm = () => {
      useDraftPodStore.setState({
        hostDisplayName: "Host",
        guestDisplayName: "Alice",
        joinCode: "NAT123",
        config: {
          ...pod().config,
          packs: [{ code: "TST", name: "Test" }, { code: "TST", name: "Test" }, { code: "TST", name: "Test" }],
        },
      });
    };

    it("creates nothing", async () => {
      const stale = admit("ws://a:9374/ws");
      admit("ws://b:9374/ws");
      seedForm();

      void pod().createPod(stale);
      // Reach: the create loaded and published its procedure.
      await vi.waitFor(() => expect(pod().procedureCacheKey).not.toBeNull());
      expect(doubles.draftProcedure).toHaveBeenCalledOnce();
      await flush();

      expect(doubles.instances).toHaveLength(0);
      expect(fetchDouble).not.toHaveBeenCalled();
      expect(doubles.hostRoom).not.toHaveBeenCalled();
      expect(doubles.joinRoom).not.toHaveBeenCalled();
    });

    it("joins nothing", async () => {
      const stale = admit("ws://a:9374/ws");
      admit("ws://b:9374/ws");
      seedForm();
      useDraftPodStore.setState({ configError: "stale" });

      void pod().joinPod(stale);
      // Reach: the join form's checks passed and cleared the error it was seeded with.
      expect(pod().configError).toBeNull();
      await flush();

      expect(doubles.instances).toHaveLength(0);
      expect(fetchDouble).not.toHaveBeenCalled();
      expect(doubles.hostRoom).not.toHaveBeenCalled();
      expect(doubles.joinRoom).not.toHaveBeenCalled();
    });

    it.each(["create", "join"] as const)("leaves the setup the store holds to %s on its own server", async (start) => {
      admit("ws://a:9374/ws");
      const held = admit("ws://b:9374/ws");
      seedForm();
      useDraftPodStore.setState({ nativeRecoveryFailure: { error: "Seat not found", routeToken: 1 } });

      if (start === "create") void pod().createPod(held);
      else void pod().joinPod(held);
      await vi.waitFor(() => expect(doubles.instances).toHaveLength(1));

      const double = doubles.instances[0]!;
      expect(double.url).toBe("ws://b:9374/ws");
      if (start === "create") expect(double.createCalls).toHaveLength(1);
      else expect(double.joinCalls).toEqual([["NAT123", "Alice", undefined]]);
      // A later explicit action supersedes the entry recovery's failure.
      expect(pod().nativeRecoveryFailure).toBeNull();
    });

    it.each(["create", "join"] as const)("a %s that fails after its setup was replaced writes nothing into the new one", async (start) => {
      seedForm();
      const operate = (setup: NativeDraftSetup) => start === "create" ? pod().createPod(setup) : pod().joinPod(setup);
      const opened = async (count: number) => {
        await vi.waitFor(() => expect(doubles.instances).toHaveLength(count));
        const double = doubles.instances[count - 1]!;
        await vi.waitFor(() => expect(double.settles).toHaveLength(1));
        return double;
      };

      // Positive: the setup still held receives its own failure.
      const first = operate(admit("ws://a:9374/ws"));
      (await opened(1)).settles[0]!.reject(new Error("Draft is full"));
      await first;
      expect(pod().configError).toBe("Draft is full");

      useDraftPodStore.setState({ configError: null });
      const replaced = admit("ws://a:9374/ws");
      const second = operate(replaced);
      const double = await opened(2);
      // Reach: the second operation built its adapter on the replaced setup's server.
      expect(double.url).toBe("ws://a:9374/ws");
      const current = admit("ws://b:9374/ws");
      double.settles[0]!.reject(new Error("Draft is full"));
      await second;

      expect(pod().nativeSetup).toBe(current);
      expect(pod().configError).toBeNull();
    });

    it.each(["its procedure read fails", "it is offline when its procedure arrives"] as const)(
      "a create whose setup was replaced while %s writes nothing into the new one",
      async (leg) => {
        seedForm();
        const run = async (setup: NativeDraftSetup, replace: boolean) => {
          const read = held<unknown>();
          doubles.draftProcedure.mockImplementationOnce(() => read.promise);
          const calls = doubles.draftProcedure.mock.calls.length;
          const creating = pod().createPod(setup);
          // Reach: the create is reading its procedure.
          await vi.waitFor(() => expect(doubles.draftProcedure).toHaveBeenCalledTimes(calls + 1));
          if (replace) admit("ws://b:9374/ws");
          if (leg === "its procedure read fails") {
            read.reject(new Error("Procedure unavailable"));
          } else {
            useConnectivityStore.setState({ forcedOffline: true });
            read.resolve(draftProcedureFixture());
          }
          await creating;
          useConnectivityStore.setState({ forcedOffline: false });
        };
        const expected = leg === "its procedure read fails" ? "Procedure unavailable" : DRAFT_OFFLINE_ERROR;

        // Positive: with no re-admission the create records its own failure.
        await run(admit("ws://a:9374/ws"), false);
        expect(pod().configError).toBe(expected);

        useDraftPodStore.setState({ configError: null });
        await run(admit("ws://a:9374/ws"), true);
        expect(pod().configError).toBeNull();
        expect(doubles.instances).toHaveLength(0);
      },
    );
  });
});
