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
});
