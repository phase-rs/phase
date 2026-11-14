import { act } from "react";
import i18n from "i18next";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore";
import { useUiStore } from "../../../stores/uiStore";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory";
import { gameStateFactory } from "../../../test/factories/gameStateFactory";
import deGame from "../../../i18n/locales/de/game.json";
import { DebugLibraryViewer } from "../DebugLibraryViewer";

const { dispatchDebug } = vi.hoisted(() => ({
  dispatchDebug: vi.fn(),
}));

vi.mock("../../card/CardImage", () => ({
  CardImage: ({ cardName }: { cardName: string }) => (
    <div aria-label={cardName} />
  ),
}));

vi.mock("../../../hooks/useGameDispatch", () => ({
  useGameDispatch: () => dispatchDebug,
}));

describe("DebugLibraryViewer focus recovery", () => {
  beforeEach(() => {
    const card = gameObjectFactory
      .sorcery()
      .params({ zone: "Library", entered_battlefield_turn: null })
      .withId(7)
      .named("Flame Jab")
      .build();
    const gameState = gameStateFactory.withObjects(card).build({
      derived: {
        debug_library_cards: [{ object_id: card.id, name: card.name }],
      },
    });

    act(() => {
      useGameStore.setState({ gameState });
      useUiStore.setState({
        debugContextMenu: null,
        debugLibraryViewer: { playerId: 0 },
      });
    });
    dispatchDebug.mockReset();
    dispatchDebug.mockImplementation(async (action) => {
      if (
        action.type === "Debug" &&
        action.data.type === "MoveToZone" &&
        action.data.data.to_zone === "Battlefield"
      ) {
        const current = useGameStore.getState().gameState;
        useGameStore.setState({
          gameState: current
            ? {
                ...current,
                derived: {
                  ...(current.derived ?? {}),
                  debug_library_cards:
                    current.derived?.debug_library_cards?.filter(
                      (card) =>
                        card.object_id !== action.data.data.object_id,
                    ) ?? [],
                },
              }
            : current,
        });
      }
      return [];
    });
  });

  afterEach(async () => {
    cleanup();
    await i18n.changeLanguage("en");
    act(() => {
      useUiStore.setState({
        debugContextMenu: null,
        debugLibraryViewer: null,
      });
    });
  });

  it("exposes the active locale's debug-action label with the card name", async () => {
    i18n.addResourceBundle("de", "game", deGame, true, true);
    await act(async () => {
      await i18n.changeLanguage("de");
    });
    render(<DebugLibraryViewer />);

    expect(
      screen.getByRole("button", {
        name: "Debug-Aktionen für Flame Jab öffnen",
      }),
    ).toBeInTheDocument();
  });

  it("recovers inside the dialog when a focused quick action is removed", async () => {
    render(<DebugLibraryViewer />);
    const dialog = screen.getByRole("dialog", { name: /Library — Player 0/ });
    await waitFor(() => expect(dialog).toHaveFocus());

    const quickMove = screen.getByRole("button", { name: "BF" });
    quickMove.focus();
    expect(quickMove).toHaveFocus();
    fireEvent.click(quickMove);

    expect(dispatchDebug).toHaveBeenCalledWith({
      type: "Debug",
      data: {
        type: "MoveToZone",
        data: { object_id: 7, to_zone: "Battlefield" },
      },
    });

    await waitFor(() =>
      expect(
        within(dialog).getByRole("button", { name: /Close Library/ }),
      ).toHaveFocus(),
    );
    expect(document.body).not.toHaveFocus();
  });
});
