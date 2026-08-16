import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { WaitingFor } from "../../../adapter/types.ts";
import { NamedChoiceModal } from "../NamedChoiceModal.tsx";

vi.mock("../ChoiceOverlay.tsx", () => ({
  ChoiceOverlay: ({
    title,
    children,
    footer,
  }: {
    title: string;
    children: React.ReactNode;
    footer?: React.ReactNode;
  }) => (
    <div>
      <h1>{title}</h1>
      {children}
      {footer}
    </div>
  ),
  ConfirmButton: ({
    onClick,
    disabled,
    label = "Confirm",
  }: {
    onClick?: () => void;
    disabled?: boolean;
    label?: string;
  }) => (
    <button type="button" onClick={onClick} disabled={disabled}>
      {label}
    </button>
  ),
}));

vi.mock("framer-motion", async (importOriginal) => {
  const actual = await importOriginal<typeof import("framer-motion")>();
  return {
    ...actual,
    motion: {
      ...actual.motion,
      button: ({
        children,
        onClick,
        className,
        style,
      }: {
        children: React.ReactNode;
        onClick?: () => void;
        className?: string;
        style?: React.CSSProperties;
      }) => (
        <button type="button" className={className} style={style} onClick={onClick}>
          {children}
        </button>
      ),
    },
  };
});

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

type NamedChoiceData = Extract<WaitingFor, { type: "NamedChoice" }>["data"];

afterEach(() => {
  cleanup();
  dispatchMock.mockReset();
});

describe("NamedChoiceModal", () => {
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

  it("shows filtered creature-type options immediately", () => {
    const creatureTypes = [
      "Ape",
      "Bear",
      "Cat",
      "Dog",
      "Elf",
      "Goblin",
      "Human",
      "Kithkin",
      "Lizard",
      "Merfolk",
      "Naga",
      "Orc",
      "Sliver",
    ];
    const data: NamedChoiceData = {
      player: 0,
      choice_type: "CreatureType",
      options: creatureTypes,
    };

    render(<NamedChoiceModal data={data} />);

    fireEvent.change(screen.getByPlaceholderText("Filter options..."), {
      target: { value: "sliver" },
    });

    expect(screen.getByRole("button", { name: "Sliver" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Ape" })).not.toBeInTheDocument();
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
