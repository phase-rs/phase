import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameState, WaitingFor } from "../../../adapter/types.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import {
  opponentMayChoiceWaitingForFactory,
  optionalEffectChoiceWaitingForFactory,
} from "../../../test/factories/gameStateFactory.ts";
import { SHORT_LANDSCAPE_QUERY } from "../../../constants/ui.ts";
import { OptionalEffectModalContent } from "../OptionalEffectModal.tsx";

const cardImage = vi.hoisted(() => ({ src: null as string | null }));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({
    src: cardImage.src,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  })),
}));

type OptionalEffectWaitingFor = Extract<
  WaitingFor,
  { type: "OptionalEffectChoice" | "OpponentMayChoice" }
>;
type OptionalEffectChoiceWaitingFor = Extract<WaitingFor, { type: "OptionalEffectChoice" }>;

function optionalWaitingFor(
  mayTriggerKey?: OptionalEffectChoiceWaitingFor["data"]["may_trigger_key"],
  sameCardAvailable = false,
): OptionalEffectChoiceWaitingFor {
  return optionalEffectChoiceWaitingForFactory
    .withData({
      player: 0,
      source_id: 100,
      description: "You may gain 1 life.",
      may_trigger_key: mayTriggerKey,
      same_card_may_trigger_choice_available: sameCardAvailable,
    })
    .build();
}

function renderModal(waitingFor: OptionalEffectWaitingFor, objects?: GameState["objects"]) {
  const dispatch = vi.fn<(action: GameAction) => void>();
  render(
    <OptionalEffectModalContent
      waitingFor={waitingFor}
      objects={objects}
      dispatch={dispatch}
    />,
  );
  return dispatch;
}

afterEach(() => {
  cleanup();
  cardImage.src = null;
});

describe("OptionalEffectModalContent", () => {
  it.each([
    ["OptionalEffectChoice", optionalEffectChoiceWaitingForFactory],
    ["OpponentMayChoice", opponentMayChoiceWaitingForFactory],
  ] as const)("titles %s from its source while previewing its decision subject", (_type, factory) => {
    const bre = gameObjectFactory
      .creature(2, 2)
      .legendary()
      .named("Bre of Clan Stoutarm")
      .withId(100)
      .build();
    const bears = gameObjectFactory
      .creature(2, 2)
      .inExile()
      .named("Grizzly Bears")
      .withId(44)
      .build();
    const waitingFor = factory
      .withData({ source_id: bre.id, decision_subject_id: bears.id })
      .build();

    renderModal(waitingFor, { [bre.id]: bre, [bears.id]: bears });

    expect(
      screen.getByRole("dialog", { name: "Bre of Clan Stoutarm - Optional Effect" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "Grizzly Bears" })).toBeInTheDocument();
    expect(screen.queryByRole("img", { name: "Bre of Clan Stoutarm" })).not.toBeInTheDocument();
  });

  it.each([undefined, 999])(
    "falls back to the source preview when the decision subject is %s",
    (decisionSubjectId) => {
      const bre = gameObjectFactory
        .creature(2, 2)
        .legendary()
        .named("Bre of Clan Stoutarm")
        .withId(100)
        .build();
      const waitingFor = optionalEffectChoiceWaitingForFactory
        .withData({ source_id: bre.id, decision_subject_id: decisionSubjectId })
        .build();

      renderModal(waitingFor, { [bre.id]: bre });

      expect(screen.getByRole("img", { name: "Bre of Clan Stoutarm" })).toBeInTheDocument();
    },
  );

  it("hides remember control for unkeyed prompts and dispatches the existing action", () => {
    const dispatch = renderModal(optionalWaitingFor());

    expect(screen.queryByLabelText("Don't ask again this game")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Yes" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffect",
      data: { accept: true },
    });
  });

  it("keeps keyed prompts on the existing action while remember is unchecked", () => {
    const dispatch = renderModal(
      optionalWaitingFor({
        player: 0,
        source_id: 100,
        origin: { type: "Printed", trigger_index: 0 },
      }),
    );

    expect(screen.getByLabelText("Don't ask again this game")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "No" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffect",
      data: { accept: false },
    });
  });

  it("dispatches remembered accept and decline actions when checked", () => {
    const dispatch = renderModal(
      optionalWaitingFor({
        player: 0,
        source_id: 100,
        origin: { type: "Printed", trigger_index: 0 },
      }),
    );

    fireEvent.click(screen.getByLabelText("Don't ask again this game"));
    fireEvent.click(screen.getByRole("button", { name: "Yes" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffectAndRemember",
      data: { choice: { type: "Accept" }, scope: { type: "ExactInstance" } },
    });

    cleanup();
    const declineDispatch = renderModal(
      optionalWaitingFor({
        player: 0,
        source_id: 100,
        origin: { type: "Printed", trigger_index: 0 },
      }),
    );

    fireEvent.click(screen.getByLabelText("Don't ask again this game"));
    fireEvent.click(screen.getByRole("button", { name: "No" }));

    expect(declineDispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffectAndRemember",
      data: { choice: { type: "Decline" }, scope: { type: "ExactInstance" } },
    });
  });

  it("shows the engine-gated same-card checkbox and dispatches that scope", () => {
    const dispatch = renderModal(
      optionalWaitingFor(
        {
          player: 0,
          source_id: 100,
          origin: { type: "Printed", trigger_index: 0 },
        },
        true,
      ),
    );

    const rememberCheckbox = screen.getByLabelText("Don't ask again this game");
    const sameCardCheckbox = screen.getByLabelText(
      "Use this choice for every copy of this card this game",
    );

    fireEvent.click(sameCardCheckbox);

    expect(sameCardCheckbox).toBeChecked();
    expect(rememberCheckbox).toBeChecked();

    fireEvent.click(screen.getByRole("button", { name: "Yes" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffectAndRemember",
      data: { choice: { type: "Accept" }, scope: { type: "SameCard" } },
    });
  });

  it("resets the same-card checkbox for each prompt", () => {
    const keyed: NonNullable<OptionalEffectChoiceWaitingFor["data"]["may_trigger_key"]> = {
      player: 0,
      source_id: 100,
      origin: { type: "Printed", trigger_index: 0 },
    };
    const { rerender } = render(
      <OptionalEffectModalContent waitingFor={optionalWaitingFor(keyed, true)} dispatch={vi.fn()} />,
    );

    const sameCardLabel = "Use this choice for every copy of this card this game";
    const rememberLabel = "Don't ask again this game";
    fireEvent.click(screen.getByLabelText(sameCardLabel));
    expect(screen.getByLabelText(sameCardLabel)).toBeChecked();
    expect(screen.getByLabelText(rememberLabel)).toBeChecked();

    rerender(
      <OptionalEffectModalContent
        waitingFor={{ ...optionalWaitingFor(keyed, true), data: { ...optionalWaitingFor(keyed, true).data, source_id: 101 } }}
        dispatch={vi.fn()}
      />,
    );
    expect(screen.getByLabelText(sameCardLabel)).not.toBeChecked();
    expect(screen.getByLabelText(rememberLabel)).not.toBeChecked();
  });
});

// Issue #11006: on a phone on its side the stacked prompt was a narrow column
// taller than the screen, with half the width unused. There it splits into two
// columns: the card on the left, and Yes and No side by side above the
// remember checkboxes on the right. happy-dom does no layout, so these pin the
// structure, against happy-dom's real `matchMedia`.
describe("OptionalEffectModalContent on a short landscape screen", () => {
  const DEFAULT_SCREEN = { innerWidth: window.innerWidth, innerHeight: window.innerHeight };
  const REMEMBER_LABEL = "Don't ask again this game";
  const SAME_CARD_LABEL = "Use this choice for every copy of this card this game";
  const HOLD_HINT = "Hold to see the full card";
  const RHYSTIC_STUDY_ART = "https://cards.example/rhystic-study.jpg";

  // Assignment, not `Object.defineProperty`: vitest's window accessors pass a
  // set on to happy-dom, whose `matchMedia` reads its own size. Redefining the
  // property would leave that size, and every media query, unchanged.
  function setScreen(width: number, height: number) {
    Object.assign(window, { innerWidth: width, innerHeight: height });
  }

  function renderRhysticStudy() {
    const rhysticStudy = gameObjectFactory
      .enchantment()
      .onBattlefield()
      .named("Rhystic Study")
      .withId(100)
      .build();
    const waitingFor = optionalEffectChoiceWaitingForFactory
      .withData({
        player: 0,
        source_id: rhysticStudy.id,
        description: "Whenever an opponent casts a spell, you may draw a card unless that player pays {1}.",
        may_trigger_key: { player: 0, source_id: rhysticStudy.id, origin: { type: "Printed", trigger_index: 0 } },
        same_card_may_trigger_choice_available: true,
      })
      .build();
    return renderModal(waitingFor, { [rhysticStudy.id]: rhysticStudy });
  }

  /** The loaded art: `CardTextboxPreview`'s `<img>` inside its band. */
  function artImage() {
    return document.querySelector<HTMLImageElement>(`img[src="${RHYSTIC_STUDY_ART}"]`);
  }

  function expectStacked(card: Element | null) {
    // Reach guard: the prompt has its card, so it is stacked by the screen,
    // not by the no-preview fallback.
    expect(card).toBeInTheDocument();
    const yes = screen.getByRole("button", { name: "Yes" });
    const options = yes.parentElement as HTMLElement;

    expect(screen.getByRole("dialog", { name: "Rhystic Study - Optional Effect" })).toHaveClass("max-w-sm");
    expect(screen.getByRole("button", { name: "No" }).parentElement).toBe(options);
    expect(options).toHaveClass("flex", "flex-col");
    expect(options).not.toHaveClass("grid-cols-2");
    expect(screen.queryByText(HOLD_HINT)).not.toBeInTheDocument();
    expect(screen.getByLabelText(REMEMBER_LABEL).closest("label")).not.toHaveClass("min-h-11");
  }

  afterEach(() => {
    Object.assign(window, DEFAULT_SCREEN);
  });

  it("puts the card on the left, and Yes and No side by side above the checkboxes on the right", () => {
    setScreen(844, 390);
    // Reach guard: the screen above is the one the layout keys on.
    expect(window.matchMedia(SHORT_LANDSCAPE_QUERY).matches).toBe(true);

    const dispatch = renderRhysticStudy();

    const yes = screen.getByRole("button", { name: "Yes" });
    const actionRow = yes.parentElement as HTMLElement;
    const actionColumn = actionRow.parentElement as HTMLElement;
    const columns = actionColumn.parentElement as HTMLElement;
    const cardColumn = columns.firstElementChild as HTMLElement;

    // The dialog widens from `max-w-sm` to hold both columns.
    expect(screen.getByRole("dialog", { name: "Rhystic Study - Optional Effect" })).toHaveClass("max-w-3xl");
    expect(columns).toHaveClass("grid", "grid-cols-2");
    expect(Array.from(columns.children)).toEqual([cardColumn, actionColumn]);
    expect(cardColumn).toContainElement(screen.getByRole("img", { name: "Rhystic Study" }));

    expect(screen.getByRole("button", { name: "No" }).parentElement).toBe(actionRow);
    expect(actionRow).toHaveClass("grid", "grid-cols-2");

    // A hint under the card says it can be held for the full card. A mouse
    // previews on hover, so the hint shows only for a coarse pointer.
    const hint = screen.getByText(HOLD_HINT);
    expect(cardColumn).toContainElement(hint);
    expect(hint).toHaveClass("hidden", "pointer-coarse:block");

    const remember = screen.getByLabelText(REMEMBER_LABEL);
    const sameCard = screen.getByLabelText(SAME_CARD_LABEL);
    expect(actionColumn).toContainElement(remember);
    expect(actionColumn).toContainElement(sameCard);
    expect(remember.closest("label")).toHaveClass("min-h-11");
    expect(sameCard.closest("label")).toHaveClass("min-h-11");

    fireEvent.click(sameCard);
    fireEvent.click(yes);

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffectAndRemember",
      data: { choice: { type: "Accept" }, scope: { type: "SameCard" } },
    });
  });

  // The test above renders the name-only placeholder that a card without art
  // gets. Almost every card has art, which renders as an `<img>` inside an
  // aspect-ratio band; that band is what the split exists to fit beside the
  // actions on a 390px-tall screen.
  it("puts the loaded card art in the left column, beside the actions and the remember toggles", () => {
    cardImage.src = RHYSTIC_STUDY_ART;
    setScreen(844, 390);
    // Reach guard: the screen above is the one the layout keys on.
    expect(window.matchMedia(SHORT_LANDSCAPE_QUERY).matches).toBe(true);

    renderRhysticStudy();

    const art = artImage();
    // Reach guard: the art loaded, so this is the band, not the placeholder.
    expect(art).toBeInTheDocument();
    expect(screen.queryByRole("img", { name: "Rhystic Study" })).not.toBeInTheDocument();

    const yes = screen.getByRole("button", { name: "Yes" });
    const actionRow = yes.parentElement as HTMLElement;
    const actionColumn = actionRow.parentElement as HTMLElement;
    const columns = actionColumn.parentElement as HTMLElement;
    const artColumn = columns.firstElementChild as HTMLElement;
    const band = art?.parentElement as HTMLElement;

    expect(screen.getByRole("dialog", { name: "Rhystic Study - Optional Effect" })).toHaveClass("max-w-3xl");
    expect(columns).toHaveClass("grid", "grid-cols-2");
    expect(Array.from(columns.children)).toEqual([artColumn, actionColumn]);
    // The band sits in the art column itself, above the hint.
    expect(band.parentElement).toBe(artColumn);
    expect(artColumn.firstElementChild).toBe(band);
    expect(artColumn).toContainElement(screen.getByText(HOLD_HINT));
    expect(actionColumn).not.toContainElement(art);

    expect(screen.getByRole("button", { name: "No" }).parentElement).toBe(actionRow);
    expect(actionRow).toHaveClass("grid", "grid-cols-2");
    expect(actionColumn).toContainElement(screen.getByLabelText(REMEMBER_LABEL));
    expect(actionColumn).toContainElement(screen.getByLabelText(SAME_CARD_LABEL));
    expect(artColumn).not.toContainElement(yes);
  });

  it.each([
    ["a desktop screen", 1280, 800],
    ["a portrait phone", 390, 844],
  ])("stays stacked on %s", (_screen, width, height) => {
    setScreen(width, height);
    // Reach guard: happy-dom's media queries see this screen, not its default.
    expect(window.matchMedia(`(width: ${width}px) and (height: ${height}px)`).matches).toBe(true);
    expect(window.matchMedia(SHORT_LANDSCAPE_QUERY).matches).toBe(false);

    renderRhysticStudy();

    expectStacked(screen.getByRole("img", { name: "Rhystic Study" }));
  });

  it("stays stacked on a short portrait window, so only landscape splits", () => {
    setScreen(400, 480);
    // Reach guard: the window is short, so only its orientation keeps it stacked.
    expect(window.matchMedia("(max-height: 500px)").matches).toBe(true);
    expect(window.matchMedia(SHORT_LANDSCAPE_QUERY).matches).toBe(false);

    renderRhysticStudy();

    expectStacked(screen.getByRole("img", { name: "Rhystic Study" }));
  });

  it("follows the phone as it turns with the prompt open, keeping its art and focus", () => {
    cardImage.src = RHYSTIC_STUDY_ART;
    setScreen(390, 844);
    renderRhysticStudy();
    const art = artImage();
    expectStacked(art);
    const yes = screen.getByRole("button", { name: "Yes" });
    const dialog = screen.getByRole("dialog", { name: "Rhystic Study - Optional Effect" });
    yes.focus();

    setScreen(844, 390);
    act(() => {
      window.dispatchEvent(new Event("resize"));
    });

    expect(dialog).toHaveClass("max-w-3xl");
    expect(yes.parentElement).toHaveClass("grid-cols-2");
    // The art moved into the left column, beside the actions.
    const columns = yes.parentElement?.parentElement?.parentElement as HTMLElement;
    expect(columns).toHaveClass("grid-cols-2");
    expect(columns.firstElementChild).toContainElement(art);
    // One element tree serves both layouts, so the turn keeps the buttons,
    // the art and the focus instead of rebuilding them.
    expect(screen.getByRole("button", { name: "Yes" })).toBe(yes);
    expect(artImage()).toBe(art);
    expect(yes).toHaveFocus();

    setScreen(390, 844);
    act(() => {
      window.dispatchEvent(new Event("resize"));
    });

    expectStacked(art);
    expect(screen.getByRole("button", { name: "Yes" })).toBe(yes);
    expect(artImage()).toBe(art);
    expect(yes).toHaveFocus();
  });
});
