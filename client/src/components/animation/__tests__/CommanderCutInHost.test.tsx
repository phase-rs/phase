import type { ComponentProps, ReactNode } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { AnimationStep } from "../../../animation/types.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildCommanderGameObject, buildGameObject } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { CommanderCutInHost } from "../CommanderCutIn.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "/commander-art.jpg", isLoading: false, isRotated: false, isFlip: false }),
}));

vi.mock("framer-motion", () => {
  function Div({
    children,
    initial: _initial,
    animate: _animate,
    exit: _exit,
    transition: _transition,
    ...props
  }: ComponentProps<"div"> & {
    children?: ReactNode;
    initial?: unknown;
    animate?: unknown;
    exit?: unknown;
    transition?: unknown;
  }) {
    return <div {...props}>{children}</div>;
  }

  function Img({
    initial: _initial,
    animate: _animate,
    transition: _transition,
    ...props
  }: ComponentProps<"img"> & {
    initial?: unknown;
    animate?: unknown;
    transition?: unknown;
  }) {
    return <img {...props} />;
  }

  return {
    AnimatePresence: ({ children }: { children: ReactNode }) => <>{children}</>,
    motion: { div: Div, h2: Div, img: Img },
    useReducedMotion: () => false,
  };
});

function castStep(objectId: number): AnimationStep {
  return {
    effects: [
      {
        event: { type: "SpellCast", data: { card_id: objectId, controller: 0, object_id: objectId } },
        duration: 500,
      },
    ],
    duration: 500,
  };
}

afterEach(() => {
  cleanup();
  useAnimationStore.setState({ activeStep: null, animationNewState: null, queue: [] });
});

describe("CommanderCutInHost", () => {
  it("renders the commander art and sliding-name surface for a commander cast", async () => {
    const commander = buildCommanderGameObject({ id: 88, name: "Atraxa, Praetors' Voice" });
    const previous = gameStateFactory.commander().withObjects(commander).build();
    const next = gameStateFactory
      .commander()
      .withObjects({ ...commander, zone: "Stack" })
      .build();
    useGameStore.setState({ gameState: previous, gameSessionGeneration: 101 });
    useAnimationStore.setState({ animationNewState: next, activeStep: castStep(88) });
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });

    const { container } = render(<CommanderCutInHost />);

    expect(await screen.findByTestId("commander-cut-in")).toHaveTextContent(
      "Atraxa, Praetors' Voice",
    );
    expect(screen.queryByText(/^commander$/i)).not.toBeInTheDocument();
    expect(container.querySelectorAll("img")).toHaveLength(2);
  });

  it("renders nothing for an ordinary spell", () => {
    const spell = buildGameObject({ id: 89, name: "Sol Ring", zone: "Stack" });
    const state = gameStateFactory.withObjects(spell).build();
    useGameStore.setState({ gameState: state, gameSessionGeneration: 102 });
    useAnimationStore.setState({ animationNewState: state, activeStep: castStep(89) });

    render(<CommanderCutInHost />);

    expect(screen.queryByTestId("commander-cut-in")).not.toBeInTheDocument();
  });
});
