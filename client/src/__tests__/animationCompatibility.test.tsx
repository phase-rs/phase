import { act, cleanup, render, waitFor } from "@testing-library/react";
import { motion } from "framer-motion";
import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("native animation compatibility", () => {
  it("cancels a real Motion animation during unmount without an unhandled rejection", async () => {
    const animate = vi.spyOn(Element.prototype, "animate");
    const cancel = vi.spyOn(window.Animation.prototype, "cancel");
    const view = render(
      <motion.div initial={{ opacity: 0 }} animate={{ opacity: 1 }} transition={{ duration: 60 }} />,
    );

    await waitFor(() => expect(animate).toHaveBeenCalled());
    const animations = animate.mock.results.map((result) => result.value as Animation);
    expect(animations.some((animation) => animation.playState === "running")).toBe(true);

    view.unmount();

    expect(animations.some((animation) => cancel.mock.contexts.includes(animation))).toBe(true);
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
    });
  });

  it("preserves native finished promises and their cancellation rejection after replay", async () => {
    const element = document.createElement("div");
    const animation = element.animate({ opacity: [0, 1] }, { duration: 60_000 });
    const firstFinished = animation.finished;
    const firstRejection = expect(firstFinished).rejects.toBeInstanceOf(window.DOMException);

    expect(animation.cancel()).toBeUndefined();

    expect(animation.finished).toBe(firstFinished);
    expect(animation.playState).toBe("idle");
    expect(animation.currentTime).toBeNull();
    await firstRejection;
    await expect(firstFinished).rejects.toHaveProperty("name", "AbortError");

    animation.play();
    const replayFinished = animation.finished;
    expect(replayFinished).not.toBe(firstFinished);
    expect(animation.playState).toBe("running");
    animation.cancel();

    expect(animation.finished).toBe(replayFinished);
    // Observe the replay only after a task turn: teardown must already have
    // handled this new promise, even before a consumer reads its rejection.
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    await expect(replayFinished).rejects.toMatchObject({ name: "AbortError" });

    animation.play();
    const completed = animation.finished;
    animation.finish();
    await expect(completed).resolves.toBe(animation);
    expect(animation.playState).toBe("finished");
  });

  it.each([
    new Error("unexpected animation failure"),
    Object.assign(new Error("not a native cancellation"), { name: "AbortError" }),
    new window.DOMException("unexpected DOM failure", "InvalidStateError"),
  ])("propagates an unexpected finished rejection: %s", async (error) => {
    const animation = new window.Animation();
    const finished = Promise.reject(error);
    Object.defineProperty(animation, "finished", { value: finished });
    const observe = vi.spyOn(finished, "catch");

    animation.cancel();

    expect(observe).toHaveBeenCalledOnce();
    await expect(observe.mock.results[0].value).rejects.toBe(error);
    expect(animation.finished).toBe(finished);
  });

  it("preserves synchronous exceptions from native cancel", () => {
    const animation = document.createElement("div").animate({ opacity: [0, 1] }, 60_000);
    const error = new Error("cancel event failed");
    vi.spyOn(animation, "dispatchEvent").mockImplementation(() => {
      throw error;
    });

    expect(() => animation.cancel()).toThrow(error);
    expect(animation.playState).toBe("idle");
  });
});
