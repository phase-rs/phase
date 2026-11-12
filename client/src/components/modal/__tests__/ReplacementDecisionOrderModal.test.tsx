import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, ObjectId, WaitingFor } from "../../../adapter/types.ts";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { ReplacementDecisionOrderModalContent } from "../ReplacementDecisionOrderModal.tsx";

type ReplacementDecisionOrderWaitingFor = Extract<
  WaitingFor,
  { type: "ReplacementDecisionOrder" }
>;

function decisionOrderWaitingFor(objects: ObjectId[]): ReplacementDecisionOrderWaitingFor {
  return {
    type: "ReplacementDecisionOrder",
    data: { player: 1, objects },
  };
}

afterEach(() => {
  cleanup();
});

describe("ReplacementDecisionOrderModalContent", () => {
  it("registers the waiting state as handled", () => {
    expect(isWaitingForHandled(decisionOrderWaitingFor([3, 7]))).toBe(true);
  });

  it("dispatches the object whose decision is made first", () => {
    const dispatch = vi.fn<(action: GameAction) => void>();
    render(
      <ReplacementDecisionOrderModalContent
        waitingFor={decisionOrderWaitingFor([3, 7])}
        objects={buildObjectMap(
          buildGameObject({ id: 3, name: "Grizzly Bears" }),
          buildGameObject({ id: 7, name: "Runeclaw Bear" }),
        )}
        dispatch={dispatch}
      />,
    );

    expect(
      screen.getByRole("heading", { name: "Choose Which Decision First" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Runeclaw Bear" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [7] },
    });
  });

  it("tells two creatures with the same name apart by what is attached to each", () => {
    const dispatch = vi.fn<(action: GameAction) => void>();
    render(
      <ReplacementDecisionOrderModalContent
        waitingFor={decisionOrderWaitingFor([3, 7])}
        objects={buildObjectMap(
          buildGameObject({ id: 3, name: "Grizzly Bears", attachments: [4] }),
          buildGameObject({ id: 7, name: "Grizzly Bears", attachments: [8, 9] }),
          buildGameObject({
            id: 4,
            name: "Hyena Umbra",
            attached_to: { type: "Object", data: 3 },
          }),
          buildGameObject({
            id: 8,
            name: "Spider Umbra",
            attached_to: { type: "Object", data: 7 },
          }),
          buildGameObject({
            id: 9,
            name: "Felidar Umbra",
            attached_to: { type: "Object", data: 7 },
          }),
        )}
        dispatch={dispatch}
      />,
    );

    expect(
      screen.getByRole("button", { name: "Grizzly Bears (With Hyena Umbra)" }),
    ).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", {
        name: "Grizzly Bears (With Spider Umbra, Felidar Umbra)",
      }),
    );

    expect(dispatch).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [7] },
    });
  });

  it("names the permanent an Aura's decision is attached to", () => {
    const dispatch = vi.fn<(action: GameAction) => void>();
    render(
      <ReplacementDecisionOrderModalContent
        waitingFor={decisionOrderWaitingFor([4, 8])}
        objects={buildObjectMap(
          buildGameObject({ id: 3, name: "Grizzly Bears", attachments: [4, 8] }),
          buildGameObject({
            id: 4,
            name: "Hyena Umbra",
            attached_to: { type: "Object", data: 3 },
          }),
          buildGameObject({
            id: 8,
            name: "Spider Umbra",
            attached_to: { type: "Object", data: 3 },
          }),
        )}
        dispatch={dispatch}
      />,
    );

    expect(
      screen.getByRole("button", { name: "Spider Umbra (On Grizzly Bears)" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Hyena Umbra (On Grizzly Bears)" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [4] },
    });
  });

  it("offers an object the viewer cannot see by a placeholder name", () => {
    const dispatch = vi.fn<(action: GameAction) => void>();
    render(
      <ReplacementDecisionOrderModalContent
        waitingFor={decisionOrderWaitingFor([3, 7])}
        objects={buildObjectMap(buildGameObject({ id: 3, name: "Grizzly Bears" }))}
        dispatch={dispatch}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Unknown object" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [7] },
    });
  });
});
