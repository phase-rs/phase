import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { NamedChoiceModal, SecretAgendaModal } from "../NamedChoiceModal.tsx";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";

const dispatchMock = vi.fn();

vi.mock("../../../services/cardNames.ts", () => ({
  getCardNames: () => Promise.resolve(["Grizzly Bears", "Runeclaw Bear"]),
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({
    src: null,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  })),
}));

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

type NamedChoiceData = Extract<WaitingFor, { type: "NamedChoice" }>["data"];

afterEach(() => {
  cleanup();
  dispatchMock.mockReset();
  useGameStore.setState({ gameState: null });
});

describe("NamedChoiceModal", () => {
  describe("creature type suggestions", () => {
    const options = ["Bear", "Dragon", "Druid", "Elf", "Faerie", "Goblin", "Human", "Merfolk", "Rogue", "Scout", "Shapeshifter", "Warrior", "Wizard"];
    const data: NamedChoiceData = {
      player: 0,
      choice_type: { CreatureType: { options: [] } },
      options,
    };

    function setup(suggestions?: string[]) {
      setGameStoreForTest({
        gameState: buildGameState({ derived: { named_choice_suggestions: suggestions } }),
      });
      render(<NamedChoiceModal data={data} />);
    }

    function labels() {
      return screen.getAllByRole("button")
        .map((button) => button.textContent)
        .filter((label) => label !== "Confirm");
    }

    it("shows engine suggestion order, searches every legal type, and restores on clearing or whitespace", () => {
      setup(["Elf", "Druid"]);
      expect(labels()).toEqual(["Elf", "Druid"]);
      const input = screen.getByRole("textbox");
      fireEvent.change(input, { target: { value: "DRAG" } });
      expect(labels()).toEqual(["Dragon"]);
      fireEvent.click(screen.getByRole("button", { name: "Dragon" }));
      fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
      expect(dispatchMock).toHaveBeenCalledExactlyOnceWith({
        type: "ChooseOption", data: { choice: "Dragon" },
      });
      fireEvent.change(input, { target: { value: "" } });
      expect(labels()).toEqual(["Elf", "Druid"]);
      fireEvent.change(input, { target: { value: "   " } });
      expect(labels()).toEqual(["Elf", "Druid"]);
    });

    it("keeps the full-domain search input mounted after no matches", () => {
      setup(["Elf"]);
      const input = screen.getByRole("textbox");
      fireEvent.change(input, { target: { value: "no such type" } });
      expect(labels()).toEqual([]);
      expect(screen.getByRole("textbox")).toBe(input);
      fireEvent.change(input, { target: { value: "wizard" } });
      expect(labels()).toEqual(["Wizard"]);
    });

    it.each([undefined, []])("falls back to the complete domain for suggestions %s", (suggestions) => {
      setup(suggestions);
      expect(labels()).toEqual(options);
      expect(screen.getByRole("textbox")).toBeInTheDocument();
    });

    it("uses the full option count for the filter threshold", () => {
      setup(["Elf"]);
      expect(screen.getByRole("textbox")).toBeInTheDocument();
      cleanup();
      const short = options.slice(0, 12);
      render(<NamedChoiceModal data={{ ...data, options: short }} />);
      expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
      // Without a search box every legal option stays on screen.
      expect(labels()).toEqual(short);
      const unsuggested = short.find((option) => option !== "Elf")!;
      fireEvent.click(screen.getByRole("button", { name: unsuggested }));
      fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
      expect(dispatchMock).toHaveBeenCalledWith({
        type: "ChooseOption",
        data: { choice: unsuggested },
      });
    });

    it("preserves the engine's explicit Oracle option order", () => {
      setGameStoreForTest({ gameState: buildGameState({ derived: {} }) });
      render(<NamedChoiceModal data={{
        player: 0,
        choice_type: { CreatureType: { options: ["Human", "Merfolk", "Goblin"] } },
        options: ["Human", "Merfolk", "Goblin"],
      }} />);
      expect(labels()).toEqual(["Human", "Merfolk", "Goblin"]);
      expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    });
  });

  it("renders engine-provided restricted color options", () => {
    const data: NamedChoiceData = {
      player: 0,
      choice_type: { Color: { excluded: ["White"] } },
      options: ["Blue", "Black", "Red", "Green"],
    };

    render(<NamedChoiceModal data={data} />);

    expect(screen.getByRole("heading", { name: "Choose a Color" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "White" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Blue" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseOption",
      data: { choice: "Blue" },
    });
  });

  // CR 608.2c + CR 608.2d: Dack Fayden's "for each of those permanents, choose
  // a different opponent" publishes the permanent each prompt assigns, and the
  // prompt shows it through the shared card display.
  it("shows the engine-published permanent a per-object player choice is for", () => {
    const bears = gameObjectFactory.creature(2, 2).named("Grizzly Bears").withId(44).build();
    const dreadmaw = gameObjectFactory.creature(6, 6).named("Colossal Dreadmaw").withId(45).build();
    useGameStore.setState({
      gameState: gameStateFactory.build({ objects: { [bears.id]: bears, [dreadmaw.id]: dreadmaw } }),
    });
    const data: NamedChoiceData = {
      player: 0,
      decision_subject_id: dreadmaw.id,
      choice_type: { Opponent: { restriction: null, distinctness: "DistinctFromPriorChoices" } },
      options: ["1", "2"],
    };

    render(<NamedChoiceModal data={data} />);

    expect(screen.getByRole("img", { name: "Colossal Dreadmaw" })).toBeInTheDocument();
    expect(screen.queryByRole("img", { name: "Grizzly Bears" })).not.toBeInTheDocument();
  });

  it("shows no subject card when the prompt publishes none", () => {
    const bears = gameObjectFactory.creature(2, 2).named("Grizzly Bears").withId(44).build();
    useGameStore.setState({ gameState: gameStateFactory.build({ objects: { [bears.id]: bears } }) });
    const data: NamedChoiceData = {
      player: 0,
      choice_type: { Opponent: { restriction: null, distinctness: "Independent" } },
      options: ["1", "2"],
    };

    render(<NamedChoiceModal data={data} />);

    expect(screen.queryByTestId("named-choice-subject")).not.toBeInTheDocument();
  });

  // CR 201.4: a closed card-name domain (Garth One-Eye) publishes its legal
  // names, so the modal offers exactly those instead of the open name search.
  it("renders a closed card-name domain as its published options", () => {
    const data: NamedChoiceData = {
      player: 0,
      choice_type: {
        CardName: {
          options: ["Disenchant", "Terror", "Black Lotus"],
          distinctness: "DistinctFromSourceHistory",
        },
      },
      options: ["Disenchant", "Black Lotus"],
    };

    render(<NamedChoiceModal data={data} />);

    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Terror" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Black Lotus" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseOption",
      data: { choice: "Black Lotus" },
    });
  });

  it("does not fade in every option in a searchable long list", () => {
    const data: NamedChoiceData = {
      player: 0,
      choice_type: { CreatureType: { options: [] } },
      options: Array.from({ length: 13 }, (_, index) => `Creature type ${index}`),
    };

    render(<NamedChoiceModal data={data} />);

    expect(screen.getByRole("button", { name: "Creature type 12" })).not.toHaveStyle({
      opacity: "0",
    });
  });

  // CR 107.1a/b. The engine publishes `free_entry` for a choice whose answer is
  // typed rather than picked, and enforces exactly those bounds. These pin that
  // the modal RENDERS the contract rather than re-deriving one: the numeric form
  // is selected by the contract's presence, and the bounds it enforces are the
  // ones it was handed.
  describe("free-entry number contract", () => {
    const numberChoice = (max: number): NamedChoiceData => ({
      player: 0,
      choice_type: { NumberRange: { min: 2 } },
      options: [],
      free_entry: { kind: "Number", min: 2, max },
    });

    it("submits a value inside the published range", () => {
      render(<NamedChoiceModal data={numberChoice(2147483647)} />);

      fireEvent.change(screen.getByRole("textbox"), {
        target: { value: "1000000" },
      });
      fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

      expect(dispatchMock).toHaveBeenCalledWith({
        type: "ChooseOption",
        data: { choice: "1000000" },
      });
    });

    // The bound comes from the contract, not from a constant in the component.
    // A modal that hard-coded the i32 ceiling would accept 500 here.
    it("refuses a value past the published maximum, whatever that maximum is", () => {
      render(<NamedChoiceModal data={numberChoice(99)} />);

      fireEvent.change(screen.getByRole("textbox"), {
        target: { value: "500" },
      });
      expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();

      // ...and the same component accepts a value the contract does allow, so
      // the assertion above is about the bound and not about the input being
      // inert.
      fireEvent.change(screen.getByRole("textbox"), {
        target: { value: "99" },
      });
      expect(screen.getByRole("button", { name: "Confirm" })).toBeEnabled();
    });

    it("refuses a value below the published minimum", () => {
      render(<NamedChoiceModal data={numberChoice(99)} />);

      fireEvent.change(screen.getByRole("textbox"), {
        target: { value: "1" },
      });
      expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    });

    // Without a contract there is nothing to type into: the choice is enumerated
    // and keeps its button grid, even though its choice type is still a range.
    it("renders the option grid when the engine publishes no contract", () => {
      const data: NamedChoiceData = {
        player: 0,
        choice_type: { NumberRange: { min: 0, max: 2 } },
        options: ["0", "1", "2"],
      };

      render(<NamedChoiceModal data={data} />);

      expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "1" })).toBeInTheDocument();
    });
  });
});

describe("SecretAgendaModal", () => {
  async function nameCard(query: string) {
    fireEvent.change(screen.getByRole("textbox"), { target: { value: query } });
    fireEvent.click(await screen.findByRole("button", { name: query }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
  }

  it("answers a hidden agenda with its one name", async () => {
    render(<SecretAgendaModal data={{ player: 0, object_id: 9, name_count: 1 }} />);

    await nameCard("Grizzly Bears");

    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseSecretAgendaNames",
      data: { names: ["Grizzly Bears"] },
    });
  });

  it("collects both double agenda names before sending one answer", async () => {
    render(<SecretAgendaModal data={{ player: 0, object_id: 9, name_count: 2 }} />);

    expect(screen.getByRole("heading", { name: /1 of 2/ })).toBeInTheDocument();
    await nameCard("Grizzly Bears");
    expect(dispatchMock).not.toHaveBeenCalled();

    expect(await screen.findByRole("heading", { name: /2 of 2/ })).toBeInTheDocument();
    await nameCard("Runeclaw Bear");

    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseSecretAgendaNames",
      data: { names: ["Grizzly Bears", "Runeclaw Bear"] },
    });
  });
});
