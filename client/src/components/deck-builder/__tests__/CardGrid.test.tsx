import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CardGrid } from "../CardGrid";
import type { ScryfallCard } from "../../../services/scryfall";

afterEach(cleanup);

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, string>) => {
      if (key === "grid.copyLimitReached") return `${opts?.name} - at copy limit`;
      if (key === "grid.atCopyLimit") return "At limit";
      if (key === "grid.addCard") return `Add ${opts?.name}`;
      if (key === "grid.notLegal") return `${opts?.name} - not legal`;
      if (key === "grid.notFormat") return `Not ${opts?.format}`;
      if (key === "grid.allFormats") return "all formats";
      return key;
    },
  }),
}));

vi.mock("../../../hooks/useLongPress", () => ({
  useLongPress: () => ({ handlers: {}, firedRef: { current: false } }),
}));

function makeCard(name: string): ScryfallCard {
  return {
    id: name.toLowerCase(),
    name,
    mana_cost: "",
    cmc: 0,
    type_line: "Artifact",
    color_identity: [],
    legalities: { modern: "legal" },
  };
}

describe("CardGrid copy-limit affordance", () => {
  it("disables add when canAddCard returns false", () => {
    const onAddCard = vi.fn();
    render(
      <CardGrid
        cards={[makeCard("Sol Ring")]}
        onAddCard={onAddCard}
        canAddCard={() => false}
        legalityFormat="Modern"
      />,
    );

    const button = screen.getByRole("button", { name: /Sol Ring/i });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("title", "Sol Ring - at copy limit");
    fireEvent.click(button);
    expect(onAddCard).not.toHaveBeenCalled();
  });

  it("adds when canAddCard allows it", () => {
    const onAddCard = vi.fn();
    render(
      <CardGrid
        cards={[makeCard("Sol Ring")]}
        onAddCard={onAddCard}
        canAddCard={() => true}
        legalityFormat="Modern"
      />,
    );

    const button = screen.getByRole("button", { name: /Sol Ring/i });
    expect(button).not.toBeDisabled();
    fireEvent.click(button);
    expect(onAddCard).toHaveBeenCalledTimes(1);
  });
});
