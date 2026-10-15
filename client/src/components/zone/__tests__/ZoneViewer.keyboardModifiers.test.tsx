import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types";
import type { ParsedItem } from "../../../hooks/useEngineCardData";
import { useKeyboardShortcuts } from "../../../hooks/useKeyboardShortcuts";
import { useGameStore } from "../../../stores/gameStore";
import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { useUiStore } from "../../../stores/uiStore";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory";
import { gameStateFactory } from "../../../test/factories/gameStateFactory";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers";
import { GameCardPreview } from "../../card/GameCardPreview";
import { ZoneViewer } from "../ZoneViewer";

const parseDetails: ParsedItem[] = [
  { category: "keyword", label: "Flying", supported: true },
];

// Keep the production image and preview components; only their data fetches
// are deterministic, as in the existing GameCardPreview tests.
vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: (cardName: string) => ({
    src: cardName ? `${cardName}.png` : null,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  }),
}));

vi.mock("../../../hooks/useEngineCardData", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: (cardName: string | null) =>
    cardName ? parseDetails : null,
  useCardRulings: () => [],
}));

vi.mock("../../../game/dispatch", () => ({ dispatchAction: vi.fn() }));

function KeyboardBinding() {
  useKeyboardShortcuts();
  return null;
}

function ZonePreviewHarness({
  keyboard = true,
  preview = true,
}: {
  keyboard?: boolean;
  preview?: boolean;
}) {
  return (
    <>
      {keyboard && <KeyboardBinding />}
      <button type="button">Outside modal</button>
      <ZoneViewer zone="graveyard" playerId={0} onClose={vi.fn()} />
      <div data-testid="preview-host">{preview && <GameCardPreview />}</div>
    </>
  );
}

async function renderInspectedZone(hasBackFace = true) {
  const backFace: NonNullable<GameObject["back_face"]> = {
    name: "Preview Back",
    power: 3,
    toughness: 3,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "NoCost" },
    keywords: [],
    abilities: [],
    color: [],
    layout_kind: "Transform",
  };
  const object = gameObjectFactory.creature().inGraveyard().named(
    "Preview Front",
  )
    .params({ keywords: ["Flying"], back_face: hasBackFace ? backFace : null })
    .build();
  setGameStoreForTest({
    gameMode: "ai",
    gameState: gameStateFactory.withPlayers(
      { id: 0, graveyard: [object.id] },
      1,
    )
      .withObjects(object).priority(0).build(),
    legalActions: [],
    legalActionsByObject: {},
    spellCosts: {},
  });
  const view = render(<ZonePreviewHarness />);
  const dialog = await screen.findByRole("dialog", { name: /Graveyard/ });
  const image = within(dialog).getByRole("img", { name: object.name });
  const source = image.closest<HTMLElement>("[data-card-hover]");
  expect(source).not.toBeNull();
  if (!source) throw new Error("The real ZoneCard hover root is missing");

  // Keep the DOM fixture's hover query deterministic so the real dismiss
  // poll keeps seeing this still-connected source.
  // Inspection itself must still be established by the real pointer handler.
  const querySelector = document.querySelector.bind(document);
  vi.spyOn(document, "querySelector").mockImplementation((selector: string) =>
    selector === "[data-card-hover]:hover" && source.isConnected
      ? source
      : querySelector(selector)
  );
  fireEvent.pointerEnter(source, { pointerType: "mouse" });
  expect(useUiStore.getState().inspectedObjectId).toBe(object.id);
  expect(useUiStore.getState().previewSticky).toBe(false);
  expect(source).toBeInTheDocument();
  const control = within(dialog).getByRole("button", { name: /Close/ });
  control.focus();
  expect(control).toHaveFocus();
  const previewHost = screen.getByTestId("preview-host");
  return { ...view, dialog, source, control, previewHost, object };
}

describe("ZoneViewer preview keyboard modifiers", () => {
  const bubbleProbe = vi.fn();
  let savedPreferences: ReturnType<typeof usePreferencesStore.getState>;
  let savedMultiplayer: ReturnType<typeof useMultiplayerStore.getState>;
  let savedWidth: number;
  let savedHeight: number;

  beforeEach(() => {
    savedPreferences = usePreferencesStore.getState();
    savedMultiplayer = useMultiplayerStore.getState();
    savedWidth = window.innerWidth;
    savedHeight = window.innerHeight;
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      writable: true,
      value: 1280,
    });
    Object.defineProperty(window, "innerHeight", {
      configurable: true,
      writable: true,
      value: 900,
    });
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
    usePreferencesStore.setState({
      cardPreviewMode: "follow",
      cardPreviewHoverDelayMs: 0,
    });
    useUiStore.getState().dismissPreview();
    useUiStore.setState({
      shiftHeld: false,
      altHeld: false,
      previewSticky: false,
      isDragging: false,
      mobileHandGesture: null,
      fullControl: false,
      flexEditMode: false,
      helpSheetOpen: false,
      debugPanelOpen: false,
      debugInteractionMode: false,
      pendingAbilityChoice: null,
    });
    bubbleProbe.mockClear();
    window.addEventListener("keydown", bubbleProbe);
  });

  afterEach(() => {
    cleanup();
    window.removeEventListener("keydown", bubbleProbe);
    useUiStore.getState().dismissPreview();
    useUiStore.setState({
      shiftHeld: false,
      isDragging: false,
      mobileHandGesture: null,
    });
    useGameStore.getState().reset();
    useMultiplayerStore.setState(savedMultiplayer);
    usePreferencesStore.setState(savedPreferences);
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      writable: true,
      value: savedWidth,
    });
    Object.defineProperty(window, "innerHeight", {
      configurable: true,
      writable: true,
      value: savedHeight,
    });
    vi.restoreAllMocks();
  });

  it("reveals the inspected preview with modal Shift and preserves release, blur and enablement", async () => {
    usePreferencesStore.setState({ cardPreviewMode: "shift" });
    const { control, source, object, previewHost } =
      await renderInspectedZone();
    fireEvent.keyDown(screen.getByRole("button", { name: "Outside modal" }), {
      key: "x",
    });
    expect(bubbleProbe).toHaveBeenCalledTimes(1);
    bubbleProbe.mockClear();
    expect(within(previewHost).queryByRole("img", { name: object.name })).not
      .toBeInTheDocument();
    fireEvent.keyDown(control, { key: "x" });
    expect(useUiStore.getState().shiftHeld).toBe(false);

    fireEvent.keyDown(control, { key: "Shift" });
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    expect(useUiStore.getState().shiftHeld).toBe(true);
    expect(useUiStore.getState().inspectedObjectId).toBe(object.id);
    expect(bubbleProbe).not.toHaveBeenCalled();
    fireEvent.keyDown(control, { key: "Shift", repeat: true });
    expect(within(previewHost).getByRole("img", { name: object.name }))
      .toBeInTheDocument();
    expect(useUiStore.getState().shiftHeld).toBe(true);

    fireEvent.keyUp(control, { key: "Shift" });
    expect(useUiStore.getState().shiftHeld).toBe(false);
    await waitFor(() =>
      expect(within(previewHost).queryByRole("img", { name: object.name })).not
        .toBeInTheDocument()
    );
    fireEvent.keyDown(control, { key: "Shift" });
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    fireEvent(window, new Event("blur"));
    expect(useUiStore.getState().shiftHeld).toBe(false);
    await waitFor(() =>
      expect(within(previewHost).queryByRole("img", { name: object.name })).not
        .toBeInTheDocument()
    );

    fireEvent.keyDown(control, { key: "Shift" });
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    act(() => usePreferencesStore.getState().setCardPreviewMode("follow"));
    expect(useUiStore.getState().shiftHeld).toBe(false);
    fireEvent.keyDown(control, { key: "Shift" });
    expect(useUiStore.getState().shiftHeld).toBe(false);
    act(() => usePreferencesStore.getState().setCardPreviewMode("shift"));
    await waitFor(() =>
      expect(within(previewHost).queryByRole("img", { name: object.name })).not
        .toBeInTheDocument()
    );
    fireEvent.keyDown(control, { key: "Shift" });
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    expect(source).toBeInTheDocument();
    expect(useUiStore.getState().inspectedObjectId).toBe(object.id);
    expect(useUiStore.getState().previewSticky).toBe(false);
    expect(bubbleProbe).not.toHaveBeenCalled();
  });

  it("toggles real parsed content with modal Alt without exposing the event to window bubbling", async () => {
    const { control, object, previewHost } = await renderInspectedZone();
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("button", { name: "Outside modal" }), {
      key: "x",
    });
    expect(bubbleProbe).toHaveBeenCalledTimes(1);
    bubbleProbe.mockClear();
    const alt = new KeyboardEvent("keydown", {
      key: "Alt",
      bubbles: true,
      cancelable: true,
    });
    fireEvent(control, alt);
    expect(useUiStore.getState().altHeld).toBe(true);
    expect(alt.defaultPrevented).toBe(true);
    expect(await within(previewHost).findByText("Engine Parse"))
      .toBeInTheDocument();
    expect(within(previewHost).getByText("Flying")).toBeInTheDocument();
    expect(within(previewHost).queryByRole("img", { name: object.name })).not
      .toBeInTheDocument();
    expect(bubbleProbe).not.toHaveBeenCalled();

    const repeat = new KeyboardEvent("keydown", {
      key: "Alt",
      repeat: true,
      bubbles: true,
      cancelable: true,
    });
    fireEvent(control, repeat);
    fireEvent.keyUp(control, { key: "Alt" });
    expect(useUiStore.getState().altHeld).toBe(true);
    expect(repeat.defaultPrevented).toBe(false);
    expect(within(previewHost).getByText("Engine Parse")).toBeInTheDocument();
    fireEvent.keyDown(control, { key: "Alt" });
    expect(useUiStore.getState().altHeld).toBe(false);
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    expect(within(previewHost).queryByText("Engine Parse")).not
      .toBeInTheDocument();
    expect(useUiStore.getState().inspectedObjectId).toBe(object.id);
    expect(bubbleProbe).not.toHaveBeenCalled();
  });

  it("shows the projected back face with modal Control and restores the front on keyup", async () => {
    const { control, object, previewHost } = await renderInspectedZone();
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("button", { name: "Outside modal" }), {
      key: "x",
    });
    expect(bubbleProbe).toHaveBeenCalledTimes(1);
    bubbleProbe.mockClear();
    fireEvent.keyDown(control, { key: "x" });
    expect(within(previewHost).getByRole("img", { name: object.name }))
      .toBeInTheDocument();

    fireEvent.keyDown(control, { key: "Control" });
    expect(
      await within(previewHost).findByRole("img", { name: "Preview Back" }),
    ).toBeInTheDocument();
    expect(within(previewHost).queryByRole("img", { name: object.name })).not
      .toBeInTheDocument();
    expect(bubbleProbe).not.toHaveBeenCalled();
    fireEvent.keyDown(control, { key: "Control", repeat: true });
    expect(within(previewHost).getByRole("img", { name: "Preview Back" }))
      .toBeInTheDocument();
    fireEvent.keyUp(control, { key: "Control" });
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    expect(within(previewHost).queryByRole("img", { name: "Preview Back" })).not
      .toBeInTheDocument();
    expect(useUiStore.getState().inspectedObjectId).toBe(object.id);
    expect(bubbleProbe).not.toHaveBeenCalled();
  });

  it("keeps a single-face preview visible when modal Control is held", async () => {
    const { control, object, previewHost } = await renderInspectedZone(false);
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    fireEvent.keyDown(control, { key: "Control" });
    expect(within(previewHost).getByRole("img", { name: object.name }))
      .toBeInTheDocument();
    expect(within(previewHost).queryByRole("img", { name: "Preview Back" })).not
      .toBeInTheDocument();
    fireEvent.keyUp(control, { key: "Control" });
    expect(within(previewHost).getByRole("img", { name: object.name }))
      .toBeInTheDocument();
  });

  it("keeps the front preview on a genuine mobile viewport when modal Control is held", async () => {
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      writable: true,
      value: 500,
    });
    const { control, object, previewHost } = await renderInspectedZone();
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    fireEvent.keyDown(control, { key: "Control" });
    expect(within(previewHost).getByRole("img", { name: object.name }))
      .toBeInTheDocument();
    expect(within(previewHost).queryByRole("img", { name: "Preview Back" })).not
      .toBeInTheDocument();
    fireEvent.keyUp(control, { key: "Control" });
    expect(within(previewHost).getByRole("img", { name: object.name }))
      .toBeInTheDocument();
  });

  it("removes Shift and Alt observers with the binding and remounts exactly one Alt toggle", async () => {
    usePreferencesStore.setState({ cardPreviewMode: "shift" });
    const { control, object, previewHost, rerender } =
      await renderInspectedZone();
    fireEvent.keyDown(control, { key: "Shift" });
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    fireEvent.keyDown(control, { key: "Alt" });
    expect(useUiStore.getState().altHeld).toBe(true);
    expect(await within(previewHost).findByText("Engine Parse"))
      .toBeInTheDocument();

    rerender(<ZonePreviewHarness keyboard={false} />);
    expect(useUiStore.getState().shiftHeld).toBe(false);
    expect(useUiStore.getState().altHeld).toBe(true);
    await waitFor(() =>
      expect(within(previewHost).queryByText("Engine Parse")).not
        .toBeInTheDocument()
    );
    fireEvent.keyDown(control, { key: "Shift" });
    expect(useUiStore.getState().shiftHeld).toBe(false);
    const inactiveAlt = new KeyboardEvent("keydown", {
      key: "Alt",
      bubbles: true,
      cancelable: true,
    });
    fireEvent(control, inactiveAlt);
    expect(useUiStore.getState().altHeld).toBe(true);
    expect(inactiveAlt.defaultPrevented).toBe(false);

    rerender(<ZonePreviewHarness />);
    fireEvent.keyDown(control, { key: "Shift" });
    expect(await within(previewHost).findByText("Engine Parse"))
      .toBeInTheDocument();
    const remountedAlt = new KeyboardEvent("keydown", {
      key: "Alt",
      bubbles: true,
      cancelable: true,
    });
    fireEvent(control, remountedAlt);
    expect(useUiStore.getState().altHeld).toBe(false);
    expect(remountedAlt.defaultPrevented).toBe(true);
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    expect(useUiStore.getState().inspectedObjectId).toBe(object.id);
  });

  it("removes the exact Control callbacks and starts a new inner preview on its front face", async () => {
    const addListener = vi.spyOn(window, "addEventListener");
    const removeListener = vi.spyOn(window, "removeEventListener");
    const { control, object, previewHost, rerender } =
      await renderInspectedZone();
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    const controlRegistration = addListener.mock.calls.find((
      [type, listener, capture],
    ) =>
      type === "keydown" && typeof listener === "function" &&
      listener.name === "handleKeyDown" && capture === true
    );
    const keyupRegistration = addListener.mock.calls.find(([type, listener]) =>
      type === "keyup" && typeof listener === "function" &&
      listener.name === "handleKeyUp"
    );
    expect(controlRegistration).toBeDefined();
    expect(keyupRegistration).toBeDefined();
    if (!controlRegistration || !keyupRegistration) {
      throw new Error("The live inner preview observers are missing");
    }
    fireEvent.keyDown(control, { key: "Control" });
    expect(
      await within(previewHost).findByRole("img", { name: "Preview Back" }),
    ).toBeInTheDocument();

    rerender(<ZonePreviewHarness preview={false} />);
    expect(removeListener).toHaveBeenCalledWith(
      "keydown",
      controlRegistration[1],
      true,
    );
    expect(removeListener).toHaveBeenCalledWith("keyup", keyupRegistration[1]);
    expect(within(previewHost).queryByRole("img")).not.toBeInTheDocument();
    fireEvent.keyDown(control, { key: "Control" });
    rerender(<ZonePreviewHarness />);
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
    expect(within(previewHost).queryByRole("img", { name: "Preview Back" })).not
      .toBeInTheDocument();
    fireEvent.keyDown(control, { key: "Control" });
    expect(
      await within(previewHost).findByRole("img", { name: "Preview Back" }),
    ).toBeInTheDocument();
    fireEvent.keyUp(control, { key: "Control" });
    expect(await within(previewHost).findByRole("img", { name: object.name }))
      .toBeInTheDocument();
  });
});
