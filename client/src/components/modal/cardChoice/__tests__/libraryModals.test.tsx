import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameObject, ObjectId } from "../../../../adapter/types.ts";
import { ScryModal } from "../libraryModals.tsx";

const dispatchMock = vi.fn();
const gameStoreMock = vi.hoisted(() => ({
  state: { gameState: null as { objects: Record<number, GameObject> } | null },
}));

vi.mock("../../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

vi.mock("../../../../stores/gameStore.ts", () => ({
  useGameStore: (selector: (state: typeof gameStoreMock.state) => unknown) =>
    selector(gameStoreMock.state),
}));

vi.mock("framer-motion", async (importOriginal) => {
  const actual = await importOriginal<typeof import("framer-motion")>();
  return {
    ...actual,
    Reorder: {
      Group: ({ children }: { children: ReactNode }) => <div>{children}</div>,
      Item: ({ children }: { children: ReactNode }) => <div>{children}</div>,
    },
  };
});

vi.mock("../../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => <div>{cardName}</div>,
}));

function card(id: ObjectId, name: string): GameObject {
  return {
    id,
    name,
    transformed: false,
    back_face: null,
    printed_ref: null,
    is_emblem: false,
    emblem_source: null,
    display_source: "Card",
    zone: "Library",
    tapped: false,
  } as unknown as GameObject;
}

function setLibraryObjects() {
  gameStoreMock.state.gameState = {
    objects: {
      1: card(1, "Island"),
      2: card(2, "Mountain"),
    },
  };
}

afterEach(() => {
  cleanup();
  dispatchMock.mockReset();
  gameStoreMock.state.gameState = null;
});

describe("ScryModal", () => {
  it("dispatches kept cards in the reordered top order", () => {
    setLibraryObjects();
    render(<ScryModal data={{ player: 0, cards: [1, 2] }} />);

    fireEvent.click(screen.getByRole("button", { name: "Island: Move later" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [2, 1] },
    });
  });

  it("omits cards moved to the bottom while preserving kept top order", () => {
    setLibraryObjects();
    render(<ScryModal data={{ player: 0, cards: [1, 2] }} />);

    fireEvent.click(screen.getByRole("button", { name: "Island: Move later" }));
    fireEvent.click(screen.getAllByRole("button", { name: "Top" })[1]);
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [2] },
    });
  });
});
