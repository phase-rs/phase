import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type {
  BracketCard,
  BracketReason,
  BracketReasonSource,
  ExplainedCombo,
} from "../../../types/bracket";
import { BracketPanelReason } from "../BracketPanelReason";

afterEach(cleanup);

const deck = (name: string): BracketCard => ({ name, role: "deck" });

const BASE = {
  official_line: "Game Changers: up to three.",
  source_document: "MTG Commander Format — Game Changers",
  source_published: "2026-02-09",
  source_url: "https://example.test/gc",
  cuts: [],
};

function reasonOf(source: BracketReasonSource): BracketReason {
  return { ...BASE, floor: "upgraded", source };
}

/** Wrappers that render with nothing to show. */
function textlessDivs(container: HTMLElement): HTMLDivElement[] {
  return Array.from(container.querySelectorAll("div")).filter((div) => div.textContent === "");
}

describe("BracketPanelReason", () => {
  it("shows the title and card names collapsed, details after a tap", () => {
    const reason = reasonOf({
        kind: "card_rule",
        axis: "game_changers",
        comparator: "LE",
        threshold: 0,
        observed: 3,
        tier_below: "core",
        allowed_below: 0,
        cards: [deck("Thassa's Oracle"), deck("Rhystic Study"), deck("Cyclonic Rift")],
        combos: [],
      });
    render(<BracketPanelReason reason={reason} onCardClick={vi.fn()} />);
    const header = screen.getByRole("button", { name: /Game Changers: 3/ });
    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByRole("button", { name: "Rhystic Study" })).toBeInTheDocument();
    expect(screen.queryByText(/allows none/)).toBeNull();

    fireEvent.click(header);
    expect(header).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("B2 allows none; this deck has 3.")).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: /MTG Commander Format — Game Changers, 2026-02-09/ }),
    ).toHaveAttribute("href", "https://example.test/gc");
  });

  it("tags a commander and hands its full comma name to onCardClick", () => {
    const onCardClick = vi.fn();
    const reason = reasonOf({
        kind: "card_rule",
        axis: "game_changers",
        comparator: "LE",
        threshold: 0,
        observed: 1,
        tier_below: "core",
        allowed_below: 0,
        cards: [{ name: "Tergrid, God of Fright", role: "commander" }],
        combos: [],
      });
    render(<BracketPanelReason reason={reason} onCardClick={onCardClick} />);
    expect(screen.getByText("Commander")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Tergrid, God of Fright" }));
    expect(onCardClick).toHaveBeenCalledWith("Tergrid, God of Fright");
  });

  it("shows an early combo with its cost and outcome", () => {
    const combo: ExplainedCombo = {
      pieces: [deck("Thassa's Oracle"), deck("Demonic Consultation")],
      outcomes: [{ kind: "wins" }],
      assemble_cost: 3,
    };
    const reason = reasonOf({
        kind: "combo_floor",
        trigger: { kind: "early_two_card", reading: "definitely_only", assemble_ceiling: 6 },
        combos: [combo],
      });
    render(<BracketPanelReason reason={reason} onCardClick={vi.fn()} />);
    expect(screen.getByText("Early two-card combo")).toBeInTheDocument();
    expect(screen.getByText(/Wins the game/)).toBeInTheDocument();
    expect(screen.getByText(/assembles for 3 mana/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Early two-card combo/ }));
    expect(
      screen.getByText(
        "B3 allows two-card combos only if they come together late (more than 6 mana).",
      ),
    ).toBeInTheDocument();
  });

  it("joins a combo's outcomes with the locale's comma", () => {
    const reason = reasonOf({
      kind: "combo_floor",
      trigger: { kind: "standalone_two_card" },
      combos: [
        {
          pieces: [deck("Piece A"), deck("Piece B")],
          outcomes: [{ kind: "wins" }, { kind: "unbounded", resource: "mana" }],
          assemble_cost: 6,
        },
      ],
    });
    render(<BracketPanelReason reason={reason} onCardClick={vi.fn()} />);
    expect(screen.getByText(/· Wins the game, Unbounded mana$/)).toBeInTheDocument();
  });

  it("leaves out the card line for a rule counted only from combo pairs", () => {
    const reason = reasonOf({
      kind: "card_rule",
      axis: "mass_land_denial",
      comparator: "GE",
      threshold: 1,
      observed: 1,
      tier_below: "upgraded",
      allowed_below: 0,
      cards: [],
      combos: [
        {
          pieces: [deck("Land Piece A"), deck("Land Piece B")],
          outcomes: [{ kind: "wins" }],
          assemble_cost: 5,
        },
      ],
    });
    const { container } = render(<BracketPanelReason reason={reason} onCardClick={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Land Piece A" })).toBeInTheDocument();
    expect(textlessDivs(container)).toHaveLength(0);
  });

  it("renders no subline for an owner answer that is not a claimed combo", () => {
    const reason = reasonOf({
      kind: "owner_combo_answer",
      declaration: { kind: "none_intended" },
    });
    const { container } = render(<BracketPanelReason reason={reason} onCardClick={vi.fn()} />);
    expect(textlessDivs(container)).toHaveLength(0);
  });

  it("words the owner's answer as the owner's", () => {
    const reason = reasonOf({
        kind: "owner_combo_answer",
        declaration: { kind: "intended", window: "early_game" },
      });
    render(<BracketPanelReason reason={reason} onCardClick={vi.fn()} />);
    expect(
      screen.getByText("You said this deck has an early-game two-card combo."),
    ).toBeInTheDocument();
  });
});
