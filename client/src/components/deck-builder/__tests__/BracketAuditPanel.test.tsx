import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { BracketAuditPanel } from "../BracketAuditPanel";
import type {
  BracketCard,
  BracketEstimate,
  BracketReason,
  CommanderBracket,
  ExplainedCombo,
} from "../../../types/bracket";
import plDeckBuilder from "../../../i18n/locales/pl/deck-builder.json";

afterEach(cleanup);

const deck = (name: string): BracketCard => ({ name, role: "deck" });

const ORACLE = "Thassa's Oracle";
const CONSULTATION = "Demonic Consultation";
const GAME_CHANGERS = [ORACLE, "Rhystic Study", "Cyclonic Rift"];

const OFFICIAL = {
  source_document: "Introducing Commander Brackets Beta",
  source_published: "2025-02-11",
  source_url: "https://example.com/brackets",
};

// Mirrors the engine test `yuriko_breaks_the_combo_then_drops_the_game_changers`
// (crates/engine/tests/integration/bracket_explanation.rs): B4 from an early
// two-card combo, then B3 -> B2 by dropping the three Game Changers.
const oracleCombo: ExplainedCombo = {
  pieces: [deck(ORACLE), deck(CONSULTATION)],
  outcomes: [{ kind: "wins" }],
  assemble_cost: 3,
};

const comboReason: BracketReason = {
  floor: "optimized",
  source: {
    kind: "combo_floor",
    trigger: { kind: "early_two_card", reading: "including_arguable", assemble_ceiling: 6 },
    combos: [oracleCombo],
  },
  official_line: "No intentional early-game two-card infinite combos.",
  ...OFFICIAL,
  cuts: [
    {
      kind: "break_combo",
      combo: oracleCombo,
      remove_one_of: [deck(ORACLE), deck(CONSULTATION)],
    },
  ],
};

const gameChangersReason: BracketReason = {
  floor: "upgraded",
  source: {
    kind: "card_rule",
    axis: "game_changers",
    comparator: "GE",
    threshold: 1,
    observed: 3,
    tier_below: "core",
    allowed_below: 0,
    cards: GAME_CHANGERS.map(deck),
    combos: [],
  },
  official_line: "Bracket 1 and 2 decks exclude Game Changers.",
  ...OFFICIAL,
  cuts: [{ kind: "remove_cards", count: 3, from: GAME_CHANGERS.map(deck) }],
};

const extraTurnsReason: BracketReason = {
  floor: "core",
  source: {
    kind: "card_rule",
    axis: "extra_turns",
    comparator: "GE",
    threshold: 1,
    observed: 1,
    tier_below: "exhibition",
    allowed_below: 0,
    cards: [deck("Time Warp")],
    combos: [],
  },
  official_line: "No intentional two-card infinite combos, mass land denial, or extra-turn cards.",
  ...OFFICIAL,
  cuts: [],
};

function makeEstimate(overrides: Partial<BracketEstimate> = {}): BracketEstimate {
  return {
    tier: "optimized",
    axes: {
      game_changers: { count: 3, contributing: GAME_CHANGERS },
      mass_land_denial: { count: 0, contributing: [] },
      extra_turns: { count: 1, contributing: ["Time Warp"] },
      efficient_tutors: { count: 0, contributing: [] },
    },
    checks: [],
    coverage: { counted: 7, resolved: 7, unresolved: [], confidence: "complete" },
    data_version: "2025-09-24-wotc",
    declaration: null,
    combo_barometer: { declaration: { kind: "undeclared" }, floor: null },
    barometers: {
      game_changers: "engine",
      extra_turns: "engine",
      mass_land_denial: "engine",
      two_card_combos: "engine",
    },
    combos: [],
    combo_checks: [],
    combo_coverage: "measured",
    combo_provenance: {
      snapshot_date: "2026-09-07",
      table_version: "combo-1",
      attribution: "Commander Spellbook",
      card_pool_version: "pool-1",
      filtered: {
        not_commander_legal: 0,
        not_ok: 0,
        template: 0,
        one_card: 0,
        three_or_more: 0,
        unknown_card: 0,
        irrelevant: 0,
        kept: 1,
        multi_zone_pieces: 0,
        duplicate_pair: 0,
      },
      omitted: [],
    },
    explanation: {
      why: [comboReason],
      steps: [
        { lands_on: "upgraded", reasons: [comboReason] },
        { lands_on: "core", reasons: [gameChangersReason] },
      ],
    },
    ...overrides,
  };
}

function renderPanel(estimate: BracketEstimate, manualBracket: CommanderBracket | null = null) {
  const ui = (e: BracketEstimate, m: typeof manualBracket) => (
    <BracketAuditPanel estimate={e} manualBracket={m} onCardClick={() => {}} />
  );
  const view = render(ui(estimate, manualBracket));
  return {
    ...view,
    update: (e: BracketEstimate, m: typeof manualBracket = manualBracket) =>
      view.rerender(ui(e, m)),
  };
}

const tab = (name: string | RegExp) => screen.getByRole("tab", { name });

describe("BracketAuditPanel", () => {
  it("opens on Why with the estimate in large type", () => {
    renderPanel(makeEstimate());
    expect(screen.getByText("B4 Optimized")).toBeInTheDocument();
    expect(tab("Why B4")).toHaveAttribute("aria-selected", "true");
    expect(tab("Bring it down")).toHaveAttribute("aria-selected", "false");
    expect(screen.getByText("Early two-card combo")).toBeInTheDocument();
  });

  it("switches to Bring it down and back with the arrow keys", () => {
    renderPanel(makeEstimate());
    tab("Why B4").focus();
    fireEvent.keyDown(tab("Why B4"), { key: "ArrowRight" });
    expect(tab("Bring it down")).toHaveAttribute("aria-selected", "true");
    expect(document.activeElement).toBe(tab("Bring it down"));
    expect(screen.getByText("Remove all 3:")).toBeInTheDocument();

    fireEvent.keyDown(tab("Bring it down"), { key: "ArrowLeft" });
    expect(tab("Why B4")).toHaveAttribute("aria-selected", "true");
    expect(document.activeElement).toBe(tab("Why B4"));
  });

  it("keeps an expanded Why reason open on that reason when the one above it goes", () => {
    const landDenialReason: BracketReason = {
      floor: "optimized",
      source: {
        kind: "card_rule",
        axis: "mass_land_denial",
        comparator: "GE",
        threshold: 1,
        observed: 1,
        tier_below: "upgraded",
        allowed_below: 0,
        cards: [deck("Armageddon")],
        combos: [],
      },
      official_line: "you should not expect to see these cards anywhere in Brackets 1-3",
      ...OFFICIAL,
      cuts: [{ kind: "remove_cards", count: 1, from: [deck("Armageddon")] }],
    };
    const withBoth = makeEstimate({
      explanation: {
        why: [landDenialReason, comboReason],
        steps: [{ lands_on: "upgraded", reasons: [landDenialReason, comboReason] }],
      },
    });
    const view = renderPanel(withBoth);
    const comboHeader = () => screen.getByRole("button", { name: /Early two-card combo/ });
    fireEvent.click(comboHeader());
    expect(comboHeader()).toHaveAttribute("aria-expanded", "true");

    view.update(makeEstimate());
    expect(comboHeader()).toHaveAttribute("aria-expanded", "true");
  });

  it("keeps the selected tab when the deck changes and shows only the footnote at B2", () => {
    const view = renderPanel(makeEstimate());
    fireEvent.click(tab("Bring it down"));
    view.update(makeEstimate({ tier: "core", explanation: { why: [], steps: [] } }));
    expect(tab("Bring it down")).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText(/B2 is as low as the estimate goes/)).toBeInTheDocument();
    expect(screen.queryByText(/^B\d$/)).not.toBeInTheDocument();
  });

  it("warns under the heading when the declaration is below the floor", () => {
    renderPanel(
      makeEstimate({
        declaration: { kind: "below_floor", floor: "optimized", raised_by: ["game_changers"] },
      }),
      3,
    );
    expect(
      screen.getByText(
        "You declared B3, but this deck can't be lower than B4. The AI plays your declared B3.",
      ),
    ).toBeInTheDocument();
  });

  it("orders the warnings above the tabs", () => {
    renderPanel(
      makeEstimate({
        declaration: { kind: "below_floor", floor: "optimized", raised_by: ["game_changers"] },
        coverage: {
          counted: 8,
          resolved: 6,
          unresolved: ["Missing A", "Missing B"],
          confidence: "partial",
        },
        barometers: { two_card_combos: "unanswered" },
      }),
      3,
    );
    const below = screen.getByText(/You declared B3/);
    const partial = screen.getByText("2 card names weren't found; counts may be low");
    const unchecked = screen.getByText(/Two-card combos weren't checked/);
    const tablist = screen.getByRole("tablist");
    const before = (a: Node, b: Node) =>
      Boolean(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);
    expect(before(below, partial)).toBe(true);
    expect(before(partial, unchecked)).toBe(true);
    expect(before(unchecked, tablist)).toBe(true);
  });

  it("lists the missing names when the partial-coverage line is tapped", () => {
    renderPanel(
      makeEstimate({
        coverage: {
          counted: 8,
          resolved: 6,
          unresolved: ["Missing A", "Missing B"],
          confidence: "partial",
        },
      }),
    );
    expect(screen.queryByText("Missing A")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /card names weren't found/ }));
    expect(screen.getByText("Missing A")).toBeInTheDocument();
    expect(screen.getByText("Missing B")).toBeInTheDocument();
  });

  it("shows the combo attribution only when combos were measured", () => {
    const view = renderPanel(makeEstimate());
    expect(
      screen.getByText("Combo data: Commander Spellbook, snapshot 2026-09-07"),
    ).toBeInTheDocument();
    view.update(makeEstimate({ combo_coverage: "unmeasured" }));
    expect(screen.queryByText(/Combo data/)).not.toBeInTheDocument();
  });

  it("says nothing raises it at the base floor and lists what rules out B1", () => {
    renderPanel(
      makeEstimate({
        tier: "core",
        explanation: { why: [extraTurnsReason], steps: [] },
      }),
    );
    expect(
      screen.getByText("Nothing in this deck raises it above B2, where every estimate starts."),
    ).toBeInTheDocument();
    expect(screen.getByText("These rule out declaring B1:")).toBeInTheDocument();
    expect(screen.getByText("Extra Turns: 1")).toBeInTheDocument();
  });

  it("keeps both tab panels in the page, wired to their tabs, with only the selected one filled", () => {
    renderPanel(makeEstimate());
    for (const [name, other] of [
      ["Why B4", "Bring it down"],
      ["Bring it down", "Why B4"],
    ] as const) {
      fireEvent.click(tab(name));
      const selected = tab(name);
      const panel = document.getElementById(selected.getAttribute("aria-controls") ?? "");
      expect(panel).not.toBeNull();
      expect(panel).toHaveAttribute("role", "tabpanel");
      expect(panel).toHaveAttribute("aria-labelledby", selected.id);
      expect(panel).not.toHaveAttribute("hidden");

      const inactive = document.getElementById(tab(other).getAttribute("aria-controls") ?? "");
      expect(inactive).not.toBeNull();
      expect(inactive).toHaveAttribute("role", "tabpanel");
      expect(inactive).toHaveAttribute("aria-labelledby", tab(other).id);
      expect(inactive).toHaveAttribute("hidden");
      expect(inactive?.textContent).toBe("");
    }
  });

  it("calls onCardClick with the full card name from a Why reason", () => {
    const clicked: string[] = [];
    render(
      <BracketAuditPanel
        estimate={makeEstimate()}
        manualBracket={null}
        onCardClick={(name) => clicked.push(name)}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: ORACLE }));
    expect(clicked).toEqual([ORACLE]);
  });

  it("renders the no-commander placeholder", () => {
    render(
      <BracketAuditPanel
        estimate={null}
        manualBracket={null}
        emptyReason="no-commander"
        onCardClick={() => {}}
      />,
    );
    expect(screen.getByText("Add a commander to see your bracket estimate.")).toBeInTheDocument();
  });

  it("points the partial-coverage button at its list", () => {
    renderPanel(
      makeEstimate({
        coverage: { counted: 8, resolved: 7, unresolved: ["Missing A"], confidence: "partial" },
      }),
    );
    const button = screen.getByRole("button", { name: /card name wasn't found/ });
    const list = document.getElementById(button.getAttribute("aria-controls") ?? "");
    expect(list).not.toBeNull();
    expect(list).toHaveAttribute("hidden");
    fireEvent.click(button);
    expect(list).not.toHaveAttribute("hidden");
    expect(list).toContainElement(screen.getByText("Missing A"));
  });

  it("no longer renders the barometer block or the breakdown toggle", () => {
    renderPanel(makeEstimate());
    expect(screen.queryByText("Barometers")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /breakdown/i })).not.toBeInTheDocument();
  });

  it("renders the Polish few plural for two missing names", async () => {
    const instance = createInstance();
    await instance.init({
      lng: "pl",
      fallbackLng: false,
      ns: ["deck-builder"],
      defaultNS: "deck-builder",
      resources: { pl: { "deck-builder": plDeckBuilder } },
      interpolation: { escapeValue: false },
    });

    render(
      <I18nextProvider i18n={instance}>
        <BracketAuditPanel
          estimate={makeEstimate({
            coverage: {
              counted: 8,
              resolved: 6,
              unresolved: ["Missing A", "Missing B"],
              confidence: "partial",
            },
          })}
          manualBracket={null}
          onCardClick={() => {}}
        />
      </I18nextProvider>,
    );
    const expected = instance.t("bracket.card.partialCoverage", { count: 2 });
    expect(expected).toBe(plDeckBuilder.bracket.card.partialCoverage_few.replace("{{count}}", "2"));
    expect(screen.getByText(expected)).toBeInTheDocument();
  });

  it("renders an empty-state placeholder when estimate is null and format is Commander", () => {
    render(<BracketAuditPanel estimate={null} manualBracket={null} onCardClick={() => {}} />);
    expect(screen.getByText("Add a commander to see your bracket estimate.")).toBeInTheDocument();
  });

  it("renders nothing for non-Commander formats", () => {
    const { container } = render(
      <BracketAuditPanel
        estimate={null}
        manualBracket={null}
        emptyReason="not-commander"
        onCardClick={() => {}}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders the card-data-unavailable placeholder", () => {
    render(
      <BracketAuditPanel
        estimate={null}
        manualBracket={null}
        emptyReason="card-data-unavailable"
        onCardClick={() => {}}
      />,
    );
    expect(
      screen.getByText("Bracket estimation needs card data, which hasn't loaded yet."),
    ).toBeInTheDocument();
  });
});
