import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ArenaBackground } from "../ArenaBackground.tsx";

const scene = vi.hoisted(() => ({ dispose: vi.fn(), create: vi.fn(), reducedMotion: false }));
vi.mock("../arenaBackgroundScene.ts", () => ({ createArenaBackgroundScene: scene.create }));
vi.mock("framer-motion", () => ({ useReducedMotion: () => scene.reducedMotion }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  scene.reducedMotion = false;
});

describe("Arena background lifecycle", () => {
  it.each(["static", "minimal", "reduced motion"])("keeps the painting without starting WebGL for %s", async (mode) => {
    scene.reducedMotion = mode === "reduced motion";
    const { container } = render(<ArenaBackground animated={mode !== "static"} quality={mode === "minimal" ? "minimal" : "full"} />);
    await act(async () => {});
    expect(scene.create).not.toHaveBeenCalled();
    expect(container.querySelector("img")?.getAttribute("src")).toContain("ocean-temple-topdown.webp");
  });

  it("replaces and disposes the scene when changing arena or weather strength", async () => {
    scene.create.mockReturnValue(scene.dispose);
    const { rerender, unmount } = render(<ArenaBackground color="White" ambiance={0} />);
    await waitFor(() => expect(scene.create).toHaveBeenCalledTimes(1));
    rerender(<ArenaBackground color="Green" ambiance={2} />);
    await waitFor(() => expect(scene.create).toHaveBeenCalledTimes(2));
    expect(scene.dispose).toHaveBeenCalledTimes(1);
    expect(scene.create.mock.calls[1].slice(1)).toEqual(["Green", "full", 1, 2]);
    unmount();
    expect(scene.dispose).toHaveBeenCalledTimes(2);
  });

  it("releases the scene when animation is disabled and when leaving the story", async () => {
    scene.create.mockReturnValue(scene.dispose);
    const { rerender, unmount } = render(<ArenaBackground />);
    await waitFor(() => expect(scene.create).toHaveBeenCalledTimes(1));
    rerender(<ArenaBackground animated={false} />);
    expect(scene.dispose).toHaveBeenCalledTimes(1);
    rerender(<ArenaBackground quality="reduced" />);
    await waitFor(() => expect(scene.create).toHaveBeenCalledTimes(2));
    expect(scene.create.mock.calls[1][2]).toBe("reduced");
    unmount();
    expect(scene.dispose).toHaveBeenCalledTimes(2);
  });
});
