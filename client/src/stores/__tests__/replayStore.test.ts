import { afterEach, describe, expect, it, vi } from "vitest";

type Step = "initialize" | "loadReplay" | "header" | "seek";
const STEPS: Step[] = ["initialize", "loadReplay", "header", "seek"];

const h = vi.hoisted(() => ({
  parkFirstAt: null as string | null,
  release: null as null | (() => void),
  built: [] as Array<Record<string, ReturnType<typeof vi.fn>>>,
}));

vi.mock("../../adapter/replay-adapter", () => ({
  ReplayAdapter: vi.fn().mockImplementation(function () {
    const index = h.built.length;
    const step = <T,>(name: string, value: T) =>
      vi.fn(() =>
        index === 0 && h.parkFirstAt === name
          ? new Promise<T>((resolve) => { h.release = () => resolve(value); })
          : Promise.resolve(value),
      );
    const adapter = {
      initialize: step("initialize", undefined),
      loadReplay: step("loadReplay", 3),
      header: step("header", {}),
      seek: step("seek", { players: [] }),
      dispose: vi.fn(),
    };
    h.built.push(adapter);
    return adapter;
  }),
}));

import { useReplayStore } from "../replayStore";
import { useGameStore } from "../gameStore";

const flush = async () => {
  for (let i = 0; i < 20; i++) await Promise.resolve();
};

describe("replayStore.loadReplay currency", () => {
  afterEach(() => {
    useReplayStore.getState().unload();
    h.built.length = 0;
    h.parkFirstAt = null;
    h.release = null;
  });

  it.each(STEPS)("unload() while the load is parked in %s disposes the adapter and publishes nothing", async (parked) => {
    h.parkFirstAt = parked;
    const load = useReplayStore.getState().loadReplay("{}");
    await flush();
    const adapter = h.built[0];
    expect(adapter[parked]).toHaveBeenCalledOnce();

    useReplayStore.getState().unload();
    h.release!();
    await expect(load).resolves.toBeUndefined();
    await flush();

    expect(adapter.dispose).toHaveBeenCalledOnce();
    for (const later of STEPS.slice(STEPS.indexOf(parked) + 1)) expect(adapter[later]).not.toHaveBeenCalled();
    expect(useReplayStore.getState()).toMatchObject({ adapter: null, isLoading: false, error: null });
    expect(useGameStore.getState().adapter).toBeNull();
    expect(useGameStore.getState().gameId).not.toBe("replay");
  });

  it("a second loadReplay supersedes a parked one and alone publishes", async () => {
    h.parkFirstAt = "initialize";
    const first = useReplayStore.getState().loadReplay("{}");
    await flush();
    await useReplayStore.getState().loadReplay("{}");
    h.release!();
    await expect(first).resolves.toBeUndefined();
    await flush();

    const [stale, current] = h.built;
    expect(stale.dispose).toHaveBeenCalledOnce();
    expect(stale.loadReplay).not.toHaveBeenCalled();
    expect(current.dispose).not.toHaveBeenCalled();
    expect(useReplayStore.getState().adapter).toBe(current);
    expect(useGameStore.getState().adapter).toBe(current);
    expect(useGameStore.getState().gameId).toBe("replay");
  });
});
