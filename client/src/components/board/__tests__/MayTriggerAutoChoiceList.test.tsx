import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";

import { MayTriggerAutoChoiceList } from "../MayTriggerAutoChoiceList.tsx";
import type { MayTriggerAutoChoiceRecord, ReplacementAutoChoiceRecord } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";

const { dispatchActionMock } = vi.hoisted(() => ({ dispatchActionMock: vi.fn() }));
vi.mock("../../../game/dispatch.ts", () => ({ dispatchAction: dispatchActionMock }));

function record(sourceId: number, accept: boolean): MayTriggerAutoChoiceRecord {
  return {
    selector: {
      type: "ExactInstance",
      data: {
        player: 0,
        source_id: sourceId,
        origin: { type: "Printed", trigger_index: 0 },
      },
    },
    choice: { type: accept ? "Accept" : "Decline" },
  };
}

function seed(records: MayTriggerAutoChoiceRecord[], replacements: ReplacementAutoChoiceRecord[] = []) {
  const source = gameObjectFactory.creature().onBattlefield().withId(50).named("Kodama of the East Tree").build();
  const gameState = gameStateFactory.withPlayers(0, 1).withObjects(source).build({
    may_trigger_auto_choices: records,
    replacement_auto_choices: replacements,
  });
  setGameStoreForTest({ gameState });
}

const replacementRecord: ReplacementAutoChoiceRecord = {
  id: `r${"a".repeat(64)}`,
  key: { player: 0, event: "LoseMana", kind: { type: "Order" }, candidates: [] },
  choice: { type: "Order", data: { order: [1, 0] } },
  descriptions: ["Convert to red", "Keep mana"],
};

describe("MayTriggerAutoChoiceList", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    dispatchActionMock.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders nothing when the viewer holds no auto-choices", () => {
    seed([]);
    const { container } = render(<MayTriggerAutoChoiceList />);
    expect(container).toBeEmptyDOMElement();
  });

  it("collapses stored auto-choices into a single chip that shows the count", () => {
    seed([record(50, true), record(51, false)]);
    render(<MayTriggerAutoChoiceList />);

    expect(screen.getByText("2")).toBeInTheDocument();
    expect(screen.queryByText("Clear all")).not.toBeInTheDocument();
    expect(screen.queryByText("Remove")).not.toBeInTheDocument();
  });

  it("reveals the removable list only after the chip is opened, labeled with the stored decision", () => {
    seed([record(50, true)]);
    render(<MayTriggerAutoChoiceList />);

    fireEvent.click(screen.getByRole("button", { name: /auto-deciding/i }));

    expect(screen.getByText("Clear all")).toBeInTheDocument();
    // Source name plus the stored decision (Accept -> "Yes").
    expect(screen.getByText(/Kodama of the East Tree — Yes/)).toBeInTheDocument();
    expect(screen.getByText("Remove")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Optional triggers (1)" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: /replacement choices/i })).not.toBeInTheDocument();
  });

  it("dispatches a Remove that echoes the stored selector verbatim", () => {
    seed([record(50, true)]);
    render(<MayTriggerAutoChoiceList />);

    fireEvent.click(screen.getByRole("button", { name: /auto-deciding/i }));
    fireEvent.click(screen.getByText("Remove"));

    expect(dispatchActionMock).toHaveBeenCalledWith({
      type: "SetMayTriggerAutoChoice",
      data: {
        op: {
          type: "Remove",
          data: {
            selector: {
              type: "ExactInstance",
              data: {
                player: 0,
                source_id: 50,
                origin: { type: "Printed", trigger_index: 0 },
              },
            },
          },
        },
      },
    });
  });

  it("dispatches ClearAll and closes the popover when Clear all is chosen", () => {
    seed([record(50, true), record(51, false)], [replacementRecord]);
    render(<MayTriggerAutoChoiceList />);

    fireEvent.click(screen.getByRole("button", { name: /auto-deciding/i }));
    fireEvent.click(screen.getByText("Clear all"));

    expect(dispatchActionMock).toHaveBeenCalledWith({
      type: "SetMayTriggerAutoChoice",
      data: { op: { type: "ClearAll" } },
    });
    expect(dispatchActionMock).toHaveBeenCalledWith({
      type: "SetReplacementAutoChoice",
      data: { selector: null },
    });
    expect(dispatchActionMock).toHaveBeenCalledTimes(2);
    expect(screen.queryByText("Clear all")).not.toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    seed([]);
    expect(screen.queryByRole("button", { name: /auto-deciding/i })).not.toBeInTheDocument();
  });

  it("shows engine replacement summaries and echoes their opaque removal ID", () => {
    seed([], [replacementRecord]);
    render(<MayTriggerAutoChoiceList />);
    fireEvent.click(screen.getByRole("button", { name: /auto-deciding/i }));
    expect(screen.getByText("Replacement: Convert to red → Keep mana")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Replacement choices (1)" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: /optional triggers/i })).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("Remove"));
    expect(dispatchActionMock).toHaveBeenCalledWith({ type: "SetReplacementAutoChoice", data: { selector: replacementRecord.id } });
    fireEvent.click(screen.getByText("Clear all"));
    expect(dispatchActionMock).toHaveBeenCalledWith({ type: "SetReplacementAutoChoice", data: { selector: null } });
  });

  it("shows both choice families with their counts and the combined chip count", () => {
    seed([record(50, true), record(51, false)], [replacementRecord]);
    render(<MayTriggerAutoChoiceList />);
    const trigger = screen.getByRole("button", { name: "Auto-deciding 3" });
    expect(trigger).toHaveAttribute("aria-haspopup", "dialog");
    fireEvent.click(trigger);

    expect(screen.getByRole("dialog", { name: "Auto-choices" })).toHaveFocus();
    expect(screen.getByRole("heading", { name: "Optional triggers (2)" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Replacement choices (1)" })).toBeInTheDocument();
    expect(screen.getByText("Kodama of the East Tree — Yes")).toBeVisible();
    expect(screen.getByText("Effect — No")).toBeVisible();
  });

  it("keeps many orders collapsed, reveals saved order, and preserves expansion after exact removal", () => {
    const replacements: ReplacementAutoChoiceRecord[] = Array.from({ length: 30 }, (_, index) => ({
      ...replacementRecord,
      id: `r${index.toString(16).padStart(64, "0")}`,
      key: { ...replacementRecord.key, candidates: [{ source_id: 100 + index }] },
      descriptions: [`Saved conversion ${index}`, `Saved preservation ${index}`],
    }));
    seed([], replacements);
    render(<MayTriggerAutoChoiceList />);
    fireEvent.click(screen.getByRole("button", { name: "Auto-deciding 30" }));

    expect(screen.getByRole("heading", { name: "Replacement choices (30)" })).toBeInTheDocument();
    for (const record of replacements) {
      expect(screen.getByText(`Replacement: ${record.descriptions.join(" → ")}`)).toBeVisible();
      for (const description of record.descriptions) {
        expect(screen.getByText(description)).not.toBeVisible();
      }
    }

    fireEvent.click(screen.getByText("Replacement: Saved conversion 29 → Saved preservation 29"));
    expect(screen.getByText("Saved conversion 29")).toBeVisible();
    expect(screen.getByText("Saved preservation 29")).toBeVisible();
    const disclosure = screen.getByText("Replacement: Saved conversion 29 → Saved preservation 29").closest("details")!;
    const orderedValues = within(disclosure).getByRole("list");
    expect(within(orderedValues).getAllByRole("listitem").map((item) => item.textContent)).toEqual(replacements[29].descriptions);
    expect(screen.getByText("Saved conversion 1")).not.toBeVisible();

    fireEvent.click(screen.getAllByRole("button", { name: "Remove" })[28]);
    expect(dispatchActionMock).toHaveBeenCalledExactlyOnceWith({
      type: "SetReplacementAutoChoice",
      data: { selector: replacements[28].id },
    });
    expect(screen.getByText("Saved conversion 28")).not.toBeVisible();
    seed([], replacements.filter((_, index) => index !== 28));
    expect(screen.queryByText("Replacement: Saved conversion 28 → Saved preservation 28")).not.toBeInTheDocument();
    expect(screen.getByText("Saved conversion 29")).toBeVisible();
    expect(screen.getByText("Saved preservation 29")).toBeVisible();
    expect(screen.getByText("Saved conversion 1")).not.toBeVisible();

    fireEvent.click(screen.getByText("Replacement: Saved conversion 29 → Saved preservation 29"));
    expect(screen.getByText("Saved conversion 29")).not.toBeVisible();
  });

  it("distinguishes identical saved decisions by source and removes only the second opaque ID", () => {
    const replacements: ReplacementAutoChoiceRecord[] = ["First prevention", "Second prevention"].map((source, index) => ({
      id: `r${index.toString(16).padStart(64, "0")}`,
      key: { player: 0, event: "GainLife", kind: { type: "OptionalBranch" }, candidates: [{ source_id: 100 + index }] },
      choice: { type: "Optional", data: { index: 1 } },
      descriptions: [`${source} — Decline`],
    }));
    seed([], replacements);
    render(<MayTriggerAutoChoiceList />);
    fireEvent.click(screen.getByRole("button", { name: "Auto-deciding 2" }));

    for (const record of replacements) {
      expect(screen.getByText(`Replacement: ${record.descriptions[0]}`)).toBeVisible();
      expect(screen.getByText(record.descriptions[0])).not.toBeVisible();
    }
    fireEvent.click(screen.getByText("Replacement: First prevention — Decline"));
    expect(screen.getByText("First prevention — Decline")).toBeVisible();
    expect(screen.getByText("Second prevention — Decline")).not.toBeVisible();
    fireEvent.click(screen.getByText("Replacement: Second prevention — Decline"));
    expect(screen.getByText("Second prevention — Decline")).toBeVisible();

    fireEvent.click(screen.getAllByRole("button", { name: "Remove" })[1]);
    expect(dispatchActionMock).toHaveBeenCalledExactlyOnceWith({
      type: "SetReplacementAutoChoice",
      data: { selector: replacements[1].id },
    });
    seed([], [replacements[0]]);
    expect(screen.queryByText("Replacement: Second prevention — Decline")).not.toBeInTheDocument();
    expect(screen.getByText("Replacement: First prevention — Decline")).toBeVisible();
    expect(screen.getByText("First prevention — Decline")).toBeVisible();
    expect(screen.getByRole("heading", { name: "Replacement choices (1)" })).toBeInTheDocument();
  });

  it.each(["Escape", "Close"])("dismisses with %s and restores chip focus", (method) => {
    seed([], [replacementRecord]);
    render(<MayTriggerAutoChoiceList />);
    const trigger = screen.getByRole("button", { name: /auto-deciding/i });
    fireEvent.click(trigger);
    expect(screen.getByRole("dialog", { name: "Auto-choices" })).toHaveFocus();

    if (method === "Escape") fireEvent.keyDown(window, { key: "Escape" });
    else fireEvent.click(screen.getByRole("button", { name: "Close" }));

    expect(screen.queryByRole("dialog", { name: "Auto-choices" })).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    expect(dispatchActionMock).not.toHaveBeenCalled();
  });
});
