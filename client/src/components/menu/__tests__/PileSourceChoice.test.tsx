import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const engine = vi.hoisted(() => ({ deckSupplyForFormat: vi.fn() }));
const compat = vi.hoisted(() => ({ evaluateDeckCompatibility: vi.fn() }));
vi.mock("../../../services/engineRuntime", () => engine);
vi.mock("../../../services/deckCompatibility", () => compat);

import type { GameFormat } from "../../../adapter/types";
import { STORAGE_KEY_PREFIX } from "../../../constants/storage";
import { DEFAULT_PILE_CHOICE, type PileChoice } from "../../../services/pileSource";
import { PileSourceChoice } from "../PileSourceChoice";

function Harness({ format, onChange }: { format: GameFormat; onChange: (next: PileChoice) => void }) {
  const [choice, setChoice] = useState<PileChoice>(DEFAULT_PILE_CHOICE);
  return (
    <PileSourceChoice
      format={format}
      value={choice.source}
      onChange={(next) => {
        setChoice(next);
        onChange(next);
      }}
    />
  );
}

async function settle() {
  await waitFor(() => expect(engine.deckSupplyForFormat).toHaveBeenCalled());
  await act(async () => {});
}

describe("PileSourceChoice", () => {
  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem(`${STORAGE_KEY_PREFIX}Pile A`, JSON.stringify({ main: [{ name: "Island", count: 80 }], sideboard: [] }));
    engine.deckSupplyForFormat.mockReset();
    compat.evaluateDeckCompatibility.mockReset();
  });
  afterEach(cleanup);

  it("offers the default and every saved deck when the host supplies the pile", async () => {
    engine.deckSupplyForFormat.mockResolvedValue("HostPile");
    render(<Harness format="Dandan" onChange={() => {}} />);
    await settle();
    const select = screen.getByRole("combobox", { name: "Pile" });
    expect(select).toHaveValue("");
    expect(screen.getByRole("option", { name: "Pile A" })).toBeInTheDocument();
    expect(engine.deckSupplyForFormat).toHaveBeenCalledWith("Dandan");
  });

  it.each(["EngineFixed", "PlayerBuilt"])("renders nothing for %s", async (supply) => {
    engine.deckSupplyForFormat.mockResolvedValue(supply);
    const { container } = render(<Harness format="Momir" onChange={() => {}} />);
    await settle();
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing while the answer is pending or after it fails", async () => {
    engine.deckSupplyForFormat.mockReturnValue(new Promise(() => {}));
    const pending = render(<Harness format="Dandan" onChange={() => {}} />);
    await settle();
    expect(pending.container).toBeEmptyDOMElement();
    pending.unmount();

    engine.deckSupplyForFormat.mockRejectedValue(new Error("no wasm"));
    const failed = render(<Harness format="Dandan" onChange={() => {}} />);
    await settle();
    expect(failed.container).toBeEmptyDOMElement();
  });

  it("reports a named pile illegal while its verdict is pending and shows the engine's refusal", async () => {
    engine.deckSupplyForFormat.mockResolvedValue("HostPile");
    let answer: (value: unknown) => void = () => {};
    compat.evaluateDeckCompatibility.mockReturnValue(new Promise((resolve) => { answer = resolve; }));
    const onChange = vi.fn();
    render(<Harness format="Dandan" onChange={onChange} />);
    await settle();

    await userEvent.setup().selectOptions(screen.getByRole("combobox", { name: "Pile" }), "Pile A");
    expect(onChange).toHaveBeenLastCalledWith({ source: { type: "SavedDeck", name: "Pile A" }, legal: false });
    await waitFor(() => expect(compat.evaluateDeckCompatibility).toHaveBeenCalled());
    expect(compat.evaluateDeckCompatibility.mock.calls[0][1]).toEqual({ selectedFormat: "Dandan" });

    await act(async () => answer({ selected_format_compatible: false, selected_format_reasons: ["Too many Islands"] }));
    expect(screen.getByRole("alert")).toHaveTextContent("Too many Islands");
    expect(onChange).toHaveBeenLastCalledWith({ source: { type: "SavedDeck", name: "Pile A" }, legal: false });
  });

  it("reports a compatible named pile legal", async () => {
    engine.deckSupplyForFormat.mockResolvedValue("HostPile");
    compat.evaluateDeckCompatibility.mockResolvedValue({ selected_format_compatible: true, selected_format_reasons: [] });
    const onChange = vi.fn();
    render(<Harness format="Dandan" onChange={onChange} />);
    await settle();

    await userEvent.setup().selectOptions(screen.getByRole("combobox", { name: "Pile" }), "Pile A");
    await waitFor(() =>
      expect(onChange).toHaveBeenLastCalledWith({ source: { type: "SavedDeck", name: "Pile A" }, legal: true }),
    );
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
