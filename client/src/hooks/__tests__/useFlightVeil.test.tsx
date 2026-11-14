import { cleanup, render, screen } from "@testing-library/react";
import { AnimatePresence, motion } from "framer-motion";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useFlightVeil } from "../useFlightVeil.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";

const OBJECT_ID = 5;

function Surface({ objectId }: { objectId: number }) {
  const hidden = useFlightVeil(objectId);
  return (
    <motion.div
      data-testid="surface"
      data-hidden={String(hidden)}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.5 }}
    />
  );
}

function Host({ show, objectId = OBJECT_ID }: { show: boolean; objectId?: number }) {
  return <AnimatePresence>{show && <Surface key="surface" objectId={objectId} />}</AnimatePresence>;
}

function hiddenAttr() {
  return screen.getByTestId("surface").getAttribute("data-hidden");
}

describe("useFlightVeil", () => {
  beforeEach(() => {
    useAnimationStore.getState().clearQueue();
  });

  afterEach(() => {
    cleanup();
    useAnimationStore.getState().clearQueue();
  });

  it("hides a surface that mounts while its object is flight-veiled", () => {
    useAnimationStore.getState().veilFlight(OBJECT_ID);

    render(<Host show />);

    expect(hiddenAttr()).toBe("true");
  });

  it("ignores a flight veil on a different object", () => {
    useAnimationStore.getState().veilFlight(OBJECT_ID + 1);

    render(<Host show />);

    expect(hiddenAttr()).toBe("false");
  });

  it("follows veil and unveil while mounted", () => {
    render(<Host show />);
    expect(hiddenAttr()).toBe("false");

    act(() => useAnimationStore.getState().veilFlight(OBJECT_ID));
    expect(hiddenAttr()).toBe("true");

    act(() => useAnimationStore.getState().unveilFlight(OBJECT_ID));
    expect(hiddenAttr()).toBe("false");
  });

  it("stays hidden through an exit that began while veiled, even once unveiled", () => {
    useAnimationStore.getState().veilFlight(OBJECT_ID);
    const { rerender } = render(<Host show />);

    rerender(<Host show={false} />);
    act(() => useAnimationStore.getState().unveilFlight(OBJECT_ID));

    // Still mounted: the exit animation is running, so the latch is what is under test.
    expect(screen.getByTestId("surface")).toBeInTheDocument();
    expect(hiddenAttr()).toBe("true");
  });

  it("releases the exit latch when the surface re-enters mid-exit", () => {
    useAnimationStore.getState().veilFlight(OBJECT_ID);
    const { rerender } = render(<Host show />);
    const surface = screen.getByTestId("surface");

    rerender(<Host show={false} />);
    rerender(<Host show />);
    act(() => useAnimationStore.getState().unveilFlight(OBJECT_ID));

    // Same node: framer reused the exiting instance, so its latch was set.
    expect(screen.getByTestId("surface")).toBe(surface);
    expect(hiddenAttr()).toBe("false");
  });

  it("stays visible through an exit that began unveiled", () => {
    const { rerender } = render(<Host show />);

    rerender(<Host show={false} />);

    expect(screen.getByTestId("surface")).toBeInTheDocument();
    expect(hiddenAttr()).toBe("false");
  });
});
