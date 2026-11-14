import { act, cleanup, render } from "@testing-library/react";
import { expect, it, vi } from "vitest";

import { ArenaBackground } from "../ArenaBackground.tsx";

const { importScene } = vi.hoisted(() => ({ importScene: vi.fn() }));
vi.mock("../arenaBackgroundScene.ts", () => {
  importScene();
  throw new Error("Arena scene module failed to load");
});
vi.mock("framer-motion", () => ({ useReducedMotion: () => false }));

it("keeps the arena painting when the scene import rejects", async () => {
  const rejections: unknown[] = [];
  const onRejection = (reason: unknown) => rejections.push(reason);
  process.on("unhandledRejection", onRejection);
  try {
    const { container } = render(<ArenaBackground />);
    await act(async () => {
      await vi.dynamicImportSettled();
    });

    expect(importScene).toHaveBeenCalledTimes(1);
    expect(rejections).toEqual([]);
    expect(container.querySelector("img")?.getAttribute("src")).toContain("ocean-temple-topdown.webp");
  } finally {
    process.off("unhandledRejection", onRejection);
    cleanup();
  }
});
