import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { StackEntry } from "../StackEntry.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import type { GameState, StackEntry as StackEntryType } from "../../../adapter/types.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildChooseXValueWaitingFor,
  buildCopyTargetSlot,
  buildGameState,
  buildPendingCast,
  buildPriorityWaitingFor,
  buildStackEntry,
  buildTargetSelectionProgress,
  copyTargetChoiceWaitingForFactory,
  copyRetargetWaitingForFactory,
  exploreChoiceWaitingForFactory,
  populateChoiceWaitingForFactory,
  retargetChoiceWaitingForFactory,
  returnAsAuraTargetWaitingForFactory,
  targetSelectionWaitingForFactory,
  triggerTargetSelectionWaitingForFactory,
} from "../../../test/factories/gameStateFactory.ts";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "/test-card.png", isLoading: false }),
}));

const { dispatchActionMock } = vi.hoisted(() => ({ dispatchActionMock: vi.fn() }));
vi.mock("../../../game/dispatch.ts", () => ({ dispatchAction: dispatchActionMock }));

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return buildGameState({
    next_object_id: 100,
    next_timestamp: 1,
    ...overrides,
  });
}

describe("StackEntry", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    dispatchActionMock.mockClear();
    // `uiStore` is a module singleton with no `reset()` action, so
    // `inspectedObjectId` otherwise leaks across rows and a row that never
    // clicks can inherit a prior row's value and pass vacuously.
    useUiStore.setState({ inspectedObjectId: null });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders the live pending_cast cost for an in-flight X spell instead of the printed base cost", () => {
    const entry: StackEntryType = buildStackEntry({
      id: 77,
      source_id: 42,
      controller: 0,
      kind: {
        type: "Spell",
        data: {
          card_id: 1,
          actual_mana_spent: 0,
        },
      },
    });
    const pendingCast = buildPendingCast({
      object_id: 42,
      card_id: 1,
      ability: { targets: [] },
      cost: { type: "Cost", shards: ["X", "Red", "Red"], generic: 0 },
    });

    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({
          id: 42,
          card_id: 1,
          name: "Crackle with Power",
          zone: "Stack",
          mana_cost: { type: "Cost", shards: ["X", "Red", "Red"], generic: 2 },
          card_types: { core_types: ["Sorcery"], subtypes: [], supertypes: [] },
          color: ["Red"],
          base_color: ["Red"],
        }),
      ),
      stack: [entry],
      waiting_for: buildChooseXValueWaitingFor({
        data: {
          player: 0,
          min: 0,
          max: 3,
          pending_cast: pendingCast,
        },
      }),
      has_pending_cast: true,
      pending_cast: pendingCast,
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
      });
    });

    render(
      <StackEntry
        entry={entry}
        index={0}
        isTop
        isPending
        cardSize={{ width: 120, height: 168 }}
      />,
    );

    expect(screen.getByAltText("X")).toBeInTheDocument();
    expect(screen.getAllByAltText("R")).toHaveLength(2);
    expect(screen.queryByAltText("2")).not.toBeInTheDocument();
  });

  it("raises its entry after a long press", () => {
    vi.useFakeTimers();
    const onHoverChange = vi.fn();
    const entry = buildStackEntry({ id: 77, source_id: 42 });

    render(
      <StackEntry
        entry={entry}
        index={0}
        isTop
        cardSize={{ width: 120, height: 168 }}
        onHoverChange={onHoverChange}
      />,
    );

    fireEvent.pointerDown(document.querySelector('[data-stack-entry="77"]')!, {
      button: 0,
      clientX: 12,
      clientY: 12,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(500));

    expect(onHoverChange).toHaveBeenCalledWith(true);
    vi.useRealTimers();
  });

  it("offers Revoke for an AllCopies yield after the source token has ceased", () => {
    // CR 400.7 + CR 704.5d: a ceased token is gone from `objects`, so the entry
    // has no live source object to read a card_id from — the menu must match the
    // standing AllCopies yield via the engine-stamped `source_card_id` instead.
    const entry: StackEntryType = buildStackEntry({
      id: 77,
      source_id: 42,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: {
          source_id: 42,
          ability: { targets: [], source_card_id: 7 },
          source_name: "Ophiomancer",
        },
      },
    });
    const gameState = createGameState({
      objects: {},
      stack: [entry],
      priority_yields: [{ player: 0, target: { AllCopies: { card_id: 7 } } }],
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(
      <StackEntry entry={entry} index={0} isTop isPending cardSize={{ width: 120, height: 168 }} />,
    );

    // The always-visible yield button opens the menu on a plain tap — no hidden
    // long-press. Its label reflects the standing yield ("Auto-passing…").
    fireEvent.click(screen.getByRole("button", { name: /auto-pass/i }));

    expect(screen.getByText("Revoke")).toBeInTheDocument();
  });

  it("labels a sourceless rule ability with the engine's name and invents nothing", () => {
    // CR 113.7 defines an ability's source; the rules for these engine-modeled
    // inherent abilities (CR 725.2 monarch, CR 726.2 initiative, CR 728.1 rad
    // counters, CR 702.179d speed) give them none. CR 113.8 instead defines an
    // ability's controller, and CR 901.8 separately does the same for
    // Planechase's planeswalking ability. `objects` holds nothing for this
    // entry, so the name has to come off the wire — this line used to fall
    // through to a literal "Unknown", which is game-facing text no rule produces.
    const entry: StackEntryType = buildStackEntry({
      id: 91,
      source_id: 0,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: { source_id: 0, ability: { targets: [] }, source_name: "Start your engines!" },
      },
    });
    const gameState = createGameState({ objects: {}, stack: [entry] });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(
      <StackEntry entry={entry} index={0} isTop isPending cardSize={{ width: 120, height: 168 }} />,
    );

    // Asserted on the image's alt text, not on rendered copy: the file-level
    // `useCardImage` mock always hands back a src, so this entry takes the
    // `<img>` branch and `sourceName` surfaces as `alt`. Same variable either
    // way — the `CardArtFallback` branch feeds it the identical string.
    expect(screen.getByAltText("Start your engines!")).toBeInTheDocument();
    expect(screen.queryByAltText("Unknown")).not.toBeInTheDocument();
  });

  it("invents no name when the wire carries none", () => {
    // The row above cannot reach the deleted `|| "Unknown"` literal: it supplies
    // a `source_name`, so the fallback chain short-circuits before the last
    // term. This row is the one that exercises it — an entry with no source
    // object AND no name, which is exactly the wire shape that produced the
    // reported blank "Unknown" card.
    //
    // An empty label is the honest answer here. Inventing game-facing text is
    // not the display layer's call, and the engine-side guard is what should
    // fail if a name ever goes missing again.
    const entry: StackEntryType = buildStackEntry({
      id: 92,
      source_id: 0,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: { source_id: 0, ability: { targets: [] }, source_name: "" },
      },
    });
    const gameState = createGameState({ objects: {}, stack: [entry] });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(
      <StackEntry entry={entry} index={0} isTop isPending cardSize={{ width: 120, height: 168 }} />,
    );

    expect(screen.queryByAltText("Unknown")).not.toBeInTheDocument();
  });

  it("shows a discoverable yield button on a triggered ability and opens the menu on tap", () => {
    const entry: StackEntryType = buildStackEntry({
      id: 88,
      source_id: 50,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: {
          source_id: 50,
          ability: { targets: [], source_card_id: 9 },
          source_name: "Bloodghast",
        },
      },
    });
    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({ id: 50, card_id: 9, name: "Bloodghast", zone: "Stack" }),
      ),
      stack: [entry],
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(<StackEntry entry={entry} index={0} isTop cardSize={{ width: 120, height: 168 }} />);

    // Discoverable: the control is present with no hidden gesture, and the menu
    // stays closed until the button is tapped.
    const button = screen.getByRole("button", { name: /auto-pass/i });
    expect(screen.queryByText("Only this one")).not.toBeInTheDocument();

    fireEvent.click(button);

    expect(screen.getByText("Only this one")).toBeInTheDocument();
    expect(screen.getByText("All copies")).toBeInTheDocument();
  });

  it("dispatches a scoped SetPriorityYield when a menu option is chosen", () => {
    const entry: StackEntryType = buildStackEntry({
      id: 88,
      source_id: 50,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: {
          source_id: 50,
          ability: { targets: [], source_card_id: 9 },
          source_name: "Bloodghast",
        },
      },
    });
    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({ id: 50, card_id: 9, name: "Bloodghast", zone: "Stack" }),
      ),
      stack: [entry],
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(<StackEntry entry={entry} index={0} isTop cardSize={{ width: 120, height: 168 }} />);

    fireEvent.click(screen.getByRole("button", { name: /auto-pass/i }));

    // Realistic pointer sequence: pointerdown fires PopoverMenu's window-level
    // outside-click listener BEFORE the click. If that listener wrongly treats a
    // menu-internal press as "outside" and closes the menu, the option unmounts
    // and its onClick never runs — the exact "pressing does nothing" symptom.
    const option = screen.getByText("All copies");
    fireEvent.pointerDown(option);
    expect(screen.getByText("All copies")).toBeInTheDocument(); // menu stayed open
    fireEvent.click(option);

    expect(dispatchActionMock).toHaveBeenCalledWith({
      type: "SetPriorityYield",
      data: { op: { type: "Add", data: { source_id: 50, scope: "AllCopies" } } },
    });

    // Observable behavior, not just that the handler ran: choosing an option
    // dismisses the menu. (The dispatch firing is necessary but not sufficient —
    // "the menu stays open" is a distinct, user-visible failure.)
    expect(screen.queryByText("All copies")).not.toBeInTheDocument();
  });

  it("does not render the yield button on a spell entry", () => {
    const entry: StackEntryType = buildStackEntry({
      id: 99,
      source_id: 60,
      controller: 0,
      kind: { type: "Spell", data: { card_id: 3, actual_mana_spent: 0 } },
    });
    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({ id: 60, card_id: 3, name: "Lightning Bolt", zone: "Stack" }),
      ),
      stack: [entry],
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(<StackEntry entry={entry} index={0} isTop cardSize={{ width: 120, height: 168 }} />);

    expect(screen.queryByRole("button", { name: /auto-pass/i })).not.toBeInTheDocument();
  });

  it("renders engine-authored repeated spell mode labels and hides empty or nonspell labels", () => {
    const spell: StackEntryType = buildStackEntry({
      id: 100,
      source_id: 70,
      controller: 0,
      kind: { type: "Spell", data: { card_id: 4, actual_mana_spent: 0 } },
    });
    const trigger: StackEntryType = buildStackEntry({
      id: 101,
      source_id: 70,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: { source_id: 70, ability: { targets: [] }, source_name: "Brotherhood's End" },
      },
    });
    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({ id: 70, card_id: 4, name: "Brotherhood's End", zone: "Stack" }),
      ),
      stack: [spell],
    });
    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });
    const details = {
      source_name: "Brotherhood's End",
      kind_label: "Spell",
      selected_mode_labels: ["~ deals 3 damage.", "~ deals 3 damage."],
    };

    const { rerender } = render(
      <StackEntry entry={spell} index={0} isTop cardSize={{ width: 120, height: 168 }} details={details} />,
    );
    const selectedModes = screen.getByRole("region", { name: "Selected modes" });
    expect(selectedModes).toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
    expect(screen.getAllByText("Brotherhood's End deals 3 damage.")).toHaveLength(2);

    rerender(
      <StackEntry entry={trigger} index={0} isTop cardSize={{ width: 120, height: 168 }} details={details} />,
    );
    expect(screen.queryByRole("region", { name: "Selected modes" })).not.toBeInTheDocument();

    rerender(
      <StackEntry
        entry={spell}
        index={0}
        isTop
        cardSize={{ width: 120, height: 168 }}
        details={{ ...details, selected_mode_labels: [] }}
      />,
    );
    expect(screen.queryByRole("region", { name: "Selected modes" })).not.toBeInTheDocument();
  });

  // Issue #4711: the warning already shows on hand and battlefield cards via
  // CardImage; a spell matters most while it is on the stack about to resolve,
  // and that was the one surface that skipped it.
  it("surfaces the unimplemented-mechanics warning for a spell on the stack", () => {
    const entry: StackEntryType = buildStackEntry({
      id: 77,
      source_id: 50,
      controller: 0,
      kind: { type: "Spell", data: { card_id: 9, actual_mana_spent: 0 } },
    });
    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({
          id: 50,
          card_id: 9,
          name: "Bloodghast",
          zone: "Stack",
          unimplemented_mechanics: ["cascade"],
        }),
      ),
      stack: [entry],
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(<StackEntry entry={entry} index={0} isTop cardSize={{ width: 120, height: 168 }} />);

    const badge = screen.getByTestId("unimplemented-mechanics-badge");
    expect(badge).toBeInTheDocument();
    expect(badge).toHaveAttribute("title", "Unimplemented: cascade");
  });

  it("shows no warning for a fully-supported spell on the stack", () => {
    // Discriminating guard: without it the test above would still pass if the
    // badge were rendered unconditionally for every stack entry.
    const entry: StackEntryType = buildStackEntry({
      id: 78,
      source_id: 51,
      controller: 0,
      kind: { type: "Spell", data: { card_id: 9, actual_mana_spent: 0 } },
    });
    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({ id: 51, card_id: 9, name: "Grizzly Bears", zone: "Stack" }),
      ),
      stack: [entry],
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(<StackEntry entry={entry} index={0} isTop cardSize={{ width: 120, height: 168 }} />);

    expect(screen.queryByTestId("unimplemented-mechanics-badge")).not.toBeInTheDocument();
  });

  // The stack must be a click surface for EVERY engine prompt whose legal set can
  // name a stack object, not just the two variants this component used to
  // hand-roll. `getWaitingForObjectChoiceIds` is the single authority; these rows
  // assert shapes, never card names, so they cover the class rather than a card.
  describe("stack entry targeting", () => {
    const CARD_SIZE = { width: 120, height: 168 };
    const ENTRY_ID = 162;
    const SOURCE_ID = 199;

    // `handleClick` inspects `entry.source_id`, not `entry.id`, so the two are
    // pinned to distinct values: an assertion on 199 also catches a regression
    // that swapped the call to `inspectObject(entry.id)`.
    const buildEntry = () =>
      buildStackEntry({ id: ENTRY_ID, source_id: SOURCE_ID, controller: 1 });

    const node = () => document.querySelector(`[data-stack-entry="${ENTRY_ID}"]`)!;
    // The ring class lives on the inner sized card div, not the motion wrapper.
    const ringHost = () => node().querySelector("div.overflow-hidden")!;

    const mount = (entry: StackEntryType, waitingFor: GameState["waiting_for"]) => {
      const gameState = createGameState({ stack: [entry], waiting_for: waitingFor });
      act(() => {
        useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
      });
      render(<StackEntry entry={entry} index={0} isTop cardSize={CARD_SIZE} />);
    };

    const objectSlot = () =>
      buildCopyTargetSlot({ legal_alternatives: [{ Object: ENTRY_ID }] });

    // Row 1 — the reported defect. A copy's only legal new target is a spell on
    // the stack (CR 707.10c "may choose new targets for the copy"). Before the
    // fix nothing on the stack was clickable and the prompt had no cancel, so
    // the game soft-locked.
    it("lights a stack entry the engine offers as a copy's new target and dispatches the choice", () => {
      const entry = buildEntry();
      mount(
        entry,
        copyRetargetWaitingForFactory.withData({ target_slots: [objectSlot()] }).build(),
      );

      expect(ringHost().className).toContain("ring-cyan-300");

      fireEvent.click(node());

      expect(dispatchActionMock).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Object: ENTRY_ID } },
      });
    });

    it("uses a compact representative's exact legal member for membership and dispatch", () => {
      const entry = buildEntry();
      const legalMemberId = ENTRY_ID + 1;
      const waitingFor = copyTargetChoiceWaitingForFactory
        .withData({ valid_targets: [legalMemberId] })
        .build();
      const gameState = createGameState({ stack: [entry], waiting_for: waitingFor });
      act(() => {
        useGameStore.setState({ gameState, waitingFor });
      });

      render(
        <StackEntry
          entry={entry}
          choiceObjectId={legalMemberId}
          index={0}
          isTop
          cardSize={CARD_SIZE}
        />,
      );

      expect(ringHost().className).toContain("ring-cyan-300");
      fireEvent.click(node());
      expect(dispatchActionMock).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Object: legalMemberId } },
      });
    });

    // Row 2 — paired reach-guard for rows 1/3/4: without it, a change that lit
    // every stack entry unconditionally would pass all three positives. The
    // prompt is addressed to the other seat, so this seat neither glows nor
    // dispatches, and the click falls through to plain inspect.
    it("leaves a stack entry inert when the engine is asking a different seat to choose", () => {
      const entry = buildEntry();
      const legalMemberId = ENTRY_ID + 1;
      const waitingFor = copyTargetChoiceWaitingForFactory
        .forPlayer(1)
        .withData({ valid_targets: [legalMemberId] })
        .build();
      const gameState = createGameState({ stack: [entry], waiting_for: waitingFor });
      act(() => {
        useGameStore.setState({ gameState, waitingFor });
      });
      render(
        <StackEntry
          entry={entry}
          choiceObjectId={legalMemberId}
          index={0}
          isTop
          cardSize={CARD_SIZE}
        />,
      );

      expect(ringHost().className).not.toContain("ring-cyan-300");

      fireEvent.click(node());

      expect(dispatchActionMock).not.toHaveBeenCalled();
      expect(useUiStore.getState().inspectedObjectId).toBe(SOURCE_ID);
    });

    // Row 3 — behavior preservation: plain target announcement (CR 601.2c) kept
    // working through the rewrite.
    it("keeps lighting a stack entry named by an ordinary target selection", () => {
      const entry = buildEntry();
      mount(
        entry,
        targetSelectionWaitingForFactory
          .withData({
            selection: buildTargetSelectionProgress({
              current_legal_targets: [{ Object: ENTRY_ID }],
            }),
          })
          .build(),
      );

      expect(ringHost().className).toContain("ring-cyan-300");

      fireEvent.click(node());

      expect(dispatchActionMock).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Object: ENTRY_ID } },
      });
    });

    it.each([
      [
        "trigger target selection",
        triggerTargetSelectionWaitingForFactory
          .withData({
            selection: buildTargetSelectionProgress({
              current_legal_targets: [{ Object: ENTRY_ID }],
            }),
          })
          .build(),
      ],
      [
        "copy target choice",
        copyTargetChoiceWaitingForFactory.withData({ valid_targets: [ENTRY_ID] }).build(),
      ],
      [
        "explore choice",
        exploreChoiceWaitingForFactory.withData({ choosable: [ENTRY_ID] }).build(),
      ],
      [
        "populate choice",
        populateChoiceWaitingForFactory.withData({ valid_tokens: [ENTRY_ID] }).build(),
      ],
      [
        "return-as-Aura target choice",
        returnAsAuraTargetWaitingForFactory
          .withData({ legal_targets: [{ Object: ENTRY_ID }] })
          .build(),
      ],
    ] as const)("lights and dispatches every remaining stack-capable selector: %s", (_name, waitingFor) => {
      const entry = buildEntry();
      mount(entry, waitingFor);

      expect(ringHost().className).toContain("ring-cyan-300");
      fireEvent.click(node());
      expect(dispatchActionMock).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Object: ENTRY_ID } },
      });
    });

    // Row 4 — behavior preservation: CR 115.7 single-target retarget (Bolt Bend
    // redirecting onto a counterspell) is still board-resolved.
    it("keeps lighting a stack entry named by a single-target retarget", () => {
      const entry = buildEntry();
      mount(
        entry,
        retargetChoiceWaitingForFactory
          .withData({ legal_new_targets: [{ Object: ENTRY_ID }] })
          .build(),
      );

      expect(ringHost().className).toContain("ring-cyan-300");

      fireEvent.click(node());

      expect(dispatchActionMock).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Object: ENTRY_ID } },
      });
    });

    // Row 5 — hostile: an `All`-scope retarget stays modal-resolved. Guards the
    // selector's `scope` narrowing, which the deleted inline block also had.
    it("does not light a stack entry for an all-scope retarget, which stays modal", () => {
      const entry = buildEntry();
      mount(
        entry,
        retargetChoiceWaitingForFactory
          .withData({
            scope: { type: "All" },
            legal_new_targets: [{ Object: ENTRY_ID }],
          })
          .build(),
      );

      expect(ringHost().className).not.toContain("ring-cyan-300");
    });

    // Row 6 — hostile: a prompt that names no objects at all. Paired guard for
    // the non-target click path; the `beforeEach` uiStore reset is what makes
    // this discriminate its own click rather than inheriting row 2's value.
    it("falls back to inspect when the current prompt names no objects", () => {
      const entry = buildEntry();
      const choiceObjectId = ENTRY_ID + 1;
      const waitingFor = buildPriorityWaitingFor();
      const gameState = createGameState({ stack: [entry], waiting_for: waitingFor });
      act(() => {
        useGameStore.setState({ gameState, waitingFor });
      });
      render(
        <StackEntry
          entry={entry}
          choiceObjectId={choiceObjectId}
          index={0}
          isTop
          cardSize={CARD_SIZE}
        />,
      );

      expect(ringHost().className).not.toContain("ring-cyan-300");

      fireEvent.click(node());

      expect(dispatchActionMock).not.toHaveBeenCalled();
      expect(useUiStore.getState().inspectedObjectId).toBe(SOURCE_ID);
    });

    // Row 7 — prompt lifecycle. At least one of this component's two prompt
    // observers must read the store's live `waitingFor` rather than the
    // snapshot's `gameState.waiting_for`, or a resolved prompt keeps glowing.
    it("drops the targeting glow when the live prompt ends, even while the snapshot still carries it", () => {
      const entry = buildEntry();
      mount(
        entry,
        targetSelectionWaitingForFactory
          .withData({
            selection: buildTargetSelectionProgress({
              current_legal_targets: [{ Object: ENTRY_ID }],
            }),
          })
          .build(),
      );

      // Reach-guard: this fixture really does glow, so the assertion below is
      // measuring the lifecycle and not a prompt that never lit up.
      expect(ringHost().className).toContain("ring-cyan-300");

      act(() => {
        useGameStore.setState({ waitingFor: { type: "GameOver", data: { winner: 0 } } });
      });

      expect(ringHost().className).not.toContain("ring-cyan-300");
      // Non-vacuity: the snapshot deliberately still holds the old prompt, so
      // the two fields have genuinely diverged and the glow followed the live one.
      expect(useGameStore.getState().gameState?.waiting_for?.type).toBe("TargetSelection");
    });
  });
});
