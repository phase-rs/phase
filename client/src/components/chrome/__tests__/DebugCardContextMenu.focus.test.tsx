import { act, useRef } from "react";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useUiStore } from "../../../stores/uiStore";
import { useGameStore } from "../../../stores/gameStore";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory";
import { gameStateFactory } from "../../../test/factories/gameStateFactory";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers";
import { ModalPanelShell } from "../../ui/ModalPanelShell";
import { DebugCardContextMenu } from "../DebugCardContextMenu";

const dispatchDebug = vi.fn();

vi.mock("../../../hooks/useGameDispatch", () => ({
  useGameDispatch: () => dispatchDebug,
}));

vi.mock("../../../hooks/useIsMobile", () => ({
  useIsMobile: () => false,
}));

function ScopedMenuHarness({
  onParentClose,
  showTrigger = true,
}: {
  onParentClose: () => void;
  showTrigger?: boolean;
}) {
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const openDebugContextMenu = useUiStore((state) => state.openDebugContextMenu);

  return (
    <>
      {/* The game renderer remains mounted in production while a zone-owned
          menu is open. Its surface discriminator must prevent a duplicate. */}
      <DebugCardContextMenu surface="game" />
      <ModalPanelShell title="Graveyard" onClose={onParentClose}>
        {showTrigger && (
          <button
            ref={triggerRef}
            type="button"
            onClick={(event) =>
              openDebugContextMenu({
                objectId: 7,
                x: event.clientX,
                y: event.clientY,
                surface: "zone-viewer",
              })
            }
          >
            Open debug actions for Flame Jab
          </button>
        )}
        <DebugCardContextMenu
          surface="zone-viewer"
          anchorRef={triggerRef}
        />
      </ModalPanelShell>
    </>
  );
}

describe("DebugCardContextMenu focus ownership", () => {
  beforeEach(() => {
    const card = gameObjectFactory
      .sorcery()
      .inGraveyard()
      .withId(7)
      .named("Flame Jab")
      .build();
    setGameStoreForTest({
      gameState: gameStateFactory.withPlayers(0, 1).withObjects(card).build(),
    });
    act(() => {
      useUiStore.setState({
        debugContextMenu: null,
        selectedObjectId: null,
      });
    });
    dispatchDebug.mockClear();
  });

  afterEach(() => {
    cleanup();
    act(() => {
      useUiStore.setState({
        debugContextMenu: null,
        selectedObjectId: null,
      });
    });
  });

  it("portals one matching menu, focuses it, and restores its exact trigger on Escape", async () => {
    const onParentClose = vi.fn();
    render(<ScopedMenuHarness onParentClose={onParentClose} />);

    const parentDialog = screen.getByRole("dialog", { name: "Graveyard" });
    await waitFor(() => expect(parentDialog).toHaveFocus());

    const trigger = screen.getByRole("button", {
      name: "Open debug actions for Flame Jab",
    });
    fireEvent.click(trigger, { clientX: 80, clientY: 120 });

    const menus = screen.getAllByRole("menu");
    expect(menus).toHaveLength(1);
    const menu = menus[0];
    expect(parentDialog).not.toContainElement(menu);
    expect(menu).toHaveAccessibleName("Flame Jab");

    const firstMenuItem = within(menu).getAllByRole("menuitem")[0];
    expect(firstMenuItem).toHaveFocus();

    fireEvent.keyDown(firstMenuItem, { key: "Escape" });

    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(parentDialog).toBeInTheDocument();
    expect(onParentClose).not.toHaveBeenCalled();
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it("closes and stays in its parent scope when an async action removes the object", async () => {
    let resolveDispatch!: (events: never[]) => void;
    dispatchDebug.mockImplementationOnce(
      () =>
        new Promise<never[]>((resolve) => {
          resolveDispatch = resolve;
        }),
    );
    render(<ScopedMenuHarness onParentClose={vi.fn()} />);
    const parentDialog = screen.getByRole("dialog", { name: "Graveyard" });
    const trigger = screen.getByRole("button", {
      name: "Open debug actions for Flame Jab",
    });
    fireEvent.click(trigger);
    const menu = screen.getByRole("menu", { name: "Flame Jab" });
    const remove = within(menu).getByRole("menuitem", { name: "Remove" });
    fireEvent.click(remove);
    expect(dispatchDebug).toHaveBeenCalledWith({
      type: "Debug",
      data: { type: "RemoveObject", data: { object_id: 7 } },
    });

    const current = useGameStore.getState().gameState!;
    act(() => {
      useGameStore.setState({
        gameState: { ...current, objects: {} },
      });
    });

    await waitFor(() => expect(menu).not.toBeInTheDocument());
    expect(useUiStore.getState().debugContextMenu).toBeNull();
    await waitFor(() => expect(parentDialog).toHaveFocus());
    expect(document.body).not.toHaveFocus();

    await act(async () => {
      resolveDispatch([]);
      await Promise.resolve();
    });
  });

  it("reaches every menu level with arrow, Home, and End keys", async () => {
    render(<ScopedMenuHarness onParentClose={vi.fn()} />);
    fireEvent.click(
      screen.getByRole("button", {
        name: "Open debug actions for Flame Jab",
      }),
    );

    const menu = screen.getByRole("menu");
    const zoneTrigger = within(menu).getByRole("menuitem", {
      name: /^Zone/,
    });
    const remove = within(menu).getByRole("menuitem", { name: "Remove" });
    expect(zoneTrigger).toHaveFocus();

    fireEvent.keyDown(zoneTrigger, { key: "ArrowDown" });
    expect(remove).toHaveFocus();
    fireEvent.keyDown(remove, { key: "ArrowDown" });
    expect(zoneTrigger).toHaveFocus();
    fireEvent.keyDown(zoneTrigger, { key: "End" });
    expect(remove).toHaveFocus();
    fireEvent.keyDown(remove, { key: "Home" });
    expect(zoneTrigger).toHaveFocus();

    fireEvent.keyDown(zoneTrigger, { key: "ArrowRight" });
    const zoneMenu = await screen.findByRole("menu", { name: /^Zone/ });
    const runEtb = within(zoneMenu).getByRole("menuitemcheckbox", {
      name: "Run ETB effects",
    });
    const firstZone = within(zoneMenu).getByRole("menuitem", {
      name: "Battlefield",
    });
    const lastZone = within(zoneMenu).getByRole("menuitem", { name: "Command" });
    await waitFor(() => expect(runEtb).toHaveFocus());

    fireEvent.keyDown(runEtb, { key: "ArrowDown" });
    expect(firstZone).toHaveFocus();
    fireEvent.keyDown(firstZone, { key: "End" });
    expect(lastZone).toHaveFocus();
    fireEvent.keyDown(lastZone, { key: "ArrowDown" });
    expect(runEtb).toHaveFocus();
    fireEvent.keyDown(runEtb, { key: "ArrowUp" });
    expect(lastZone).toHaveFocus();

    fireEvent.keyDown(lastZone, { key: "ArrowLeft" });
    await waitFor(() =>
      expect(screen.queryByRole("menu", { name: /^Zone/ })).not.toBeInTheDocument(),
    );
    expect(zoneTrigger).toHaveFocus();
  });

  it.each([
    { toggled: false, simulate: false },
    { toggled: true, simulate: true },
  ])(
    "moves zones with simulate=$simulate when Run ETB effects is toggled=$toggled",
    async ({ toggled, simulate }) => {
      dispatchDebug.mockResolvedValue([]);
      render(<ScopedMenuHarness onParentClose={vi.fn()} />);
      fireEvent.click(
        screen.getByRole("button", {
          name: "Open debug actions for Flame Jab",
        }),
      );
      fireEvent.click(screen.getByRole("menuitem", { name: /^Zone/ }));
      const zoneMenu = await screen.findByRole("menu", { name: /^Zone/ });
      const runEtb = within(zoneMenu).getByRole("menuitemcheckbox", {
        name: "Run ETB effects",
      });
      expect(runEtb).toHaveAttribute("aria-checked", "false");
      if (toggled) {
        fireEvent.click(runEtb);
        expect(runEtb).toHaveAttribute("aria-checked", "true");
      }

      fireEvent.click(
        within(zoneMenu).getByRole("menuitem", { name: "Battlefield" }),
      );

      await waitFor(() =>
        expect(dispatchDebug).toHaveBeenCalledWith({
          type: "Debug",
          data: {
            type: "MoveToZone",
            data: { object_id: 7, to_zone: "Battlefield", simulate },
          },
        }),
      );
    },
  );

  it("traverses standard, counter, edited P/T, and keyword commands within their owning menu", async () => {
    const creature = gameObjectFactory
      .creature(3, 4)
      .onBattlefield()
      .withId(7)
      .named("Flame Jab")
      .params({ counters: { P1P1: 2 }, keywords: ["Flying"] })
      .build();
    setGameStoreForTest({
      gameState: gameStateFactory
        .withPlayers(0, 1)
        .withObjects(creature)
        .build(),
    });
    render(<ScopedMenuHarness onParentClose={vi.fn()} />);
    fireEvent.click(
      screen.getByRole("button", {
        name: "Open debug actions for Flame Jab",
      }),
    );

    const menu = screen.getByRole("menu", { name: "Flame Jab" });
    const zoneTrigger = within(menu).getByRole("menuitem", {
      name: /^Zone/,
    });
    const tap = within(menu).getByRole("menuitem", { name: "Tap" });
    fireEvent.keyDown(zoneTrigger, { key: "ArrowDown" });
    expect(tap).toHaveFocus();

    const editPowerToughness = within(menu).getByRole("menuitem", {
      name: /Set Base P\/T/,
    });
    fireEvent.click(editPowerToughness);
    const powerInput = within(menu).getAllByRole("spinbutton")[0];
    expect(powerInput).toHaveFocus();

    const setPowerToughness = within(menu).getByRole("menuitem", {
      name: "Set",
    });
    const removeCounter = within(menu).getByRole("menuitem", {
      name: "P1P1: −1",
    });
    const addCounter = within(menu).getByRole("menuitem", {
      name: "P1P1: +1",
    });
    setPowerToughness.focus();
    fireEvent.keyDown(setPowerToughness, { key: "ArrowDown" });
    expect(removeCounter).toHaveFocus();
    fireEvent.keyDown(removeCounter, { key: "ArrowDown" });
    expect(addCounter).toHaveFocus();

    const keywordTrigger = within(menu).getByRole("menuitem", {
      name: /^Keywords/,
    });
    fireEvent.keyDown(addCounter, { key: "ArrowDown" });
    expect(keywordTrigger).toHaveFocus();

    fireEvent.keyDown(keywordTrigger, { key: "ArrowRight" });
    const keywordMenu = await screen.findByRole("menu", { name: /^Keywords/ });
    const keywordCommands = within(keywordMenu).getAllByRole(
      "menuitemcheckbox",
    );
    await waitFor(() => expect(keywordCommands[0]).toHaveFocus());
    expect(keywordCommands[0]).toHaveAttribute("aria-checked", "true");
    expect(keywordCommands[1]).toHaveAttribute("aria-checked", "false");

    fireEvent.keyDown(keywordCommands[0], { key: "ArrowDown" });
    expect(keywordCommands[1]).toHaveFocus();
    fireEvent.keyDown(keywordCommands[1], { key: "End" });
    const lastKeywordCommand = keywordCommands[keywordCommands.length - 1];
    expect(lastKeywordCommand).toHaveFocus();

    fireEvent.keyDown(lastKeywordCommand, { key: "ArrowLeft" });
    await waitFor(() =>
      expect(
        screen.queryByRole("menu", { name: /^Keywords/ }),
      ).not.toBeInTheDocument(),
    );
    expect(keywordTrigger).toHaveFocus();
  });

  it("falls back inside the parent modal when the captured trigger disconnects", async () => {
    const onParentClose = vi.fn();
    const view = render(
      <ScopedMenuHarness onParentClose={onParentClose} />,
    );

    const parentDialog = screen.getByRole("dialog", { name: "Graveyard" });
    await waitFor(() => expect(parentDialog).toHaveFocus());

    fireEvent.click(
      screen.getByRole("button", {
        name: "Open debug actions for Flame Jab",
      }),
    );
    const firstMenuItem = within(screen.getByRole("menu")).getAllByRole(
      "menuitem",
    )[0];
    expect(firstMenuItem).toHaveFocus();

    view.rerender(
      <ScopedMenuHarness onParentClose={onParentClose} showTrigger={false} />,
    );
    expect(firstMenuItem).toHaveFocus();

    fireEvent.keyDown(firstMenuItem, { key: "Escape" });

    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(onParentClose).not.toHaveBeenCalled();
    await waitFor(() => expect(parentDialog).toHaveFocus());
  });

  it("keeps the game surface's standalone focus and Escape dismissal", () => {
    render(
      <>
        <button type="button">Game card</button>
        <DebugCardContextMenu surface="game" />
      </>,
    );
    const launcher = screen.getByRole("button", { name: "Game card" });
    launcher.focus();
    act(() => {
      useUiStore.getState().openDebugContextMenu({
        objectId: 7,
        x: 80,
        y: 120,
        surface: "game",
      });
    });

    expect(screen.getByRole("menu")).toBeInTheDocument();
    expect(launcher).toHaveFocus();
    fireEvent.keyDown(window, { key: "Escape" });

    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(launcher).toHaveFocus();
  });
});
