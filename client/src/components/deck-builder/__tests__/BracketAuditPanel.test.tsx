import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { BracketAuditPanel } from "../BracketAuditPanel";
import type { BracketEstimate, ComboMatch } from "../../../types/bracket";

afterEach(cleanup);

const estimate: BracketEstimate = {
  tier: "upgraded",
  axes: {
    game_changers: {
      count: 2,
      contributing: ["Smothering Tithe", "Cyclonic Rift"],
    },
    mass_land_denial: { count: 0, contributing: [] },
    extra_turns: { count: 1, contributing: ["Time Warp"] },
    efficient_tutors: {
      count: 3,
      contributing: ["Demonic Tutor", "Vampiric Tutor", "Enlightened Tutor"],
    },
  },
  checks: [
    {
      axis: "game_changers",
      comparator: "GE",
      threshold: 1,
      floor: "upgraded",
      observed: 2,
      outcome: { kind: "fired" },
      official_line: "Bracket 1 and 2 decks exclude Game Changers.",
      source_document: "MTG Commander Format — Game Changers",
      source_published: "2026-02-09",
      source_url: "https://magic.wizards.com/en/formats/commander",
      evidence: ["Smothering Tithe", "Cyclonic Rift"],
    },
    {
      axis: "game_changers",
      comparator: "GE",
      threshold: 4,
      floor: "optimized",
      observed: 2,
      outcome: { kind: "clear", cards_until_fired: 2 },
      official_line: "Brackets 4 and 5 allow for unlimited Game Changers.",
      source_document: "MTG Commander Format — Game Changers",
      source_published: "2026-02-09",
      source_url: "https://magic.wizards.com/en/formats/commander",
      evidence: ["Smothering Tithe", "Cyclonic Rift"],
    },
    {
      axis: "mass_land_denial",
      comparator: "GE",
      threshold: 1,
      floor: "optimized",
      observed: 0,
      outcome: { kind: "clear", cards_until_fired: 1 },
      official_line: "you should not expect to see these cards anywhere in Brackets 1-3",
      source_document: "Introducing Commander Brackets Beta",
      source_published: "2025-02-11",
      source_url:
        "https://magic.wizards.com/en/news/announcements/introducing-commander-brackets-beta",
      evidence: [],
    },
    {
      axis: "extra_turns",
      comparator: "GE",
      threshold: 1,
      floor: "core",
      observed: 1,
      outcome: { kind: "fired" },
      official_line:
        "No intentional two-card infinite combos, mass land denial, or extra-turn cards.",
      source_document: "Introducing Commander Brackets Beta",
      source_published: "2025-02-11",
      source_url:
        "https://magic.wizards.com/en/news/announcements/introducing-commander-brackets-beta",
      evidence: ["Time Warp"],
    },
  ],
  coverage: {
    counted: 8,
    resolved: 7,
    unresolved: ["Missing Card"],
    confidence: "partial",
  },
  data_version: "2025-09-24-wotc",
  declaration: null,
  combo_barometer: { declaration: { kind: "undeclared" }, floor: null },
  barometers: {
    game_changers: "engine",
    extra_turns: "engine",
    mass_land_denial: "engine",
    two_card_combos: "unanswered",
  },
};

describe("BracketAuditPanel", () => {
  it("renders the not-measured line when combo coverage is unmeasured", () => {
    render(
      <BracketAuditPanel
        estimate={{ ...estimate, combo_coverage: "unmeasured" }}
        manualBracket={null}
        onCardClick={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(
      screen.getByText("Two-card combo barometer not measured in this build"),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("No two-card infinite combos found in this deck"),
    ).not.toBeInTheDocument();
    expect(screen.queryByText(/Combo data:/)).not.toBeInTheDocument();
  });

  it("renders none-found plus attribution when measured with no matches", () => {
    render(
      <BracketAuditPanel
        estimate={{
          ...estimate,
          combos: [],
          combo_checks: [],
          combo_coverage: "measured",
          combo_provenance: {
            snapshot_date: "2026-09-01",
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
              kept: 0,
              multi_zone_pieces: 0,
              duplicate_pair: 0,
            },
            omitted: [],
          },
        }}
        manualBracket={null}
        onCardClick={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(screen.getByText("No two-card infinite combos found in this deck")).toBeInTheDocument();
    expect(
      screen.getByText("Combo data: Commander Spellbook, snapshot 2026-09-01"),
    ).toBeInTheDocument();
  });

  it("renders a fired combo check with both card names, outcomes and attribution", () => {
    const onCardClick = vi.fn();
    const combo: ComboMatch = {
      pieces: [
        { key: "heliod-sun-crowned", display: "Heliod, Sun-Crowned", zone: "library" },
        { key: "walking-ballista", display: "Walking Ballista", zone: "library" },
      ],
      relevance: "standalone",
      cardinality: "definitely_two_card",
      assemble_cost: 6,
      popularity: 100,
      outcomes: [{ kind: "wins" }, { kind: "unbounded", resource: "damage" }],
      axes: [],
      source: "combo_pair",
    };
    render(
      <BracketAuditPanel
        estimate={{
          ...estimate,
          combos: [combo],
          combo_coverage: "measured",
          combo_checks: [
            {
              trigger: { kind: "standalone_two_card" },
              floor: "upgraded",
              outcome: { kind: "fired" },
              official_line: "No intentional two-card infinite combos.",
              source_document: "Introducing Commander Brackets Beta",
              source_published: "2025-02-11",
              source_url: "https://example.com/brackets",
              evidence: [combo],
            },
          ],
          combo_provenance: {
            snapshot_date: "2026-09-01",
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
            omitted: ["prerequisite_text"],
          },
        }}
        manualBracket={null}
        onCardClick={onCardClick}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(screen.getByText("Heliod, Sun-Crowned")).toBeInTheDocument();
    expect(screen.getByText("Walking Ballista")).toBeInTheDocument();
    expect(screen.getByText(/Wins the game/)).toBeInTheDocument();
    expect(screen.getByText("Unbounded damage")).toBeInTheDocument();
    expect(
      screen.getByText("Combo data: Commander Spellbook, snapshot 2026-09-01"),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Heliod, Sun-Crowned" }));
    expect(onCardClick).toHaveBeenCalledWith("Heliod, Sun-Crowned");
  });

  it("unmeasured and none-found produce different text", () => {
    const { rerender } = render(
      <BracketAuditPanel
        estimate={{ ...estimate, combo_coverage: "unmeasured" }}
        manualBracket={null}
        onCardClick={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(screen.getByText(/not measured in this build/i)).toBeInTheDocument();

    rerender(
      <BracketAuditPanel
        estimate={{ ...estimate, combos: [], combo_coverage: "measured" }}
        manualBracket={null}
        onCardClick={() => {}}
      />,
    );
    expect(screen.getByText(/No two-card infinite combos found/i)).toBeInTheDocument();
    expect(screen.queryByText(/not measured in this build/i)).not.toBeInTheDocument();
  });

  it("appends the combo-floor note to the below-floor warning", () => {
    render(
      <BracketAuditPanel
        estimate={{
          ...estimate,
          declaration: {
            kind: "below_floor",
            floor: "optimized",
            raised_by: [],
            raised_by_combo_floor: { kind: "standalone_two_card" },
          },
        }}
        manualBracket={2}
        onCardClick={() => {}}
      />,
    );
    expect(screen.getByText(/below B4 floor.*raised by a two-card combo/i)).toBeInTheDocument();
  });

  it("renders pair-sourced mass land denial under the axis", () => {
    render(
      <BracketAuditPanel
        estimate={{
          ...estimate,
          axes: {
            ...estimate.axes,
            mass_land_denial: {
              ...estimate.axes.mass_land_denial,
              combo_pairs: [["Kormus Bell", "Urborg, Tomb of Yawgmoth"]],
            },
          },
        }}
        manualBracket={null}
        onCardClick={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(
      screen.getByText("Kormus Bell + Urborg, Tomb of Yawgmoth (from the combo table)"),
    ).toBeInTheDocument();
  });

  it("renders the estimated tier chip", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={null} onCardClick={() => {}} />);
    expect(screen.getByText(/Estimated:/i)).toHaveTextContent("B3");
    expect(screen.getByText(/Upgraded/i)).toBeInTheDocument();
  });

  it("renders an unanswered combo barometer without a checked affordance", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={null} onCardClick={() => {}} />);
    const row = screen.getByText("Two-Card Infinite Combos").closest("div");
    expect(row).toHaveTextContent("Not answered");
    expect(row).not.toHaveTextContent("Checked by phase");
    expect(row).not.toHaveTextContent("✓");
  });

  it("attributes an optimized combo floor to the deck owner and names B4", () => {
    render(
      <BracketAuditPanel
        estimate={{
          ...estimate,
          combo_barometer: {
            declaration: { kind: "intended", window: "early_game" },
            floor: "optimized",
          },
          barometers: { ...estimate.barometers, two_card_combos: "deck_owner" },
        }}
        manualBracket={null}
        onCardClick={() => {}}
      />,
    );
    const row = screen.getByText("Two-Card Infinite Combos").closest("div");
    expect(row).toHaveTextContent("Declared by you");
    expect(row).toHaveTextContent("forces B4");
  });

  it("shows no warning for a cEDH declaration at or above the floor", () => {
    render(
      <BracketAuditPanel
        estimate={{ ...estimate, declaration: { kind: "at_or_above_floor" } }}
        manualBracket={5}
        onCardClick={() => {}}
      />,
    );
    expect(screen.queryByText(/below B/i)).not.toBeInTheDocument();
  });

  it("shows the engine-provided floor when the declaration is below it", () => {
    render(
      <BracketAuditPanel
        estimate={{
          ...estimate,
          declaration: {
            kind: "below_floor",
            floor: "optimized",
            raised_by: ["mass_land_denial"],
          },
        }}
        manualBracket={2}
        onCardClick={() => {}}
      />,
    );
    expect(screen.getByText(/below B4 floor/i)).toBeInTheDocument();
  });

  it("does not infer a warning when the engine provides no declaration verdict", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={2} onCardClick={() => {}} />);
    expect(screen.queryByText(/below B/i)).not.toBeInTheDocument();
  });

  it("expands to show per-axis breakdown", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={null} onCardClick={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(screen.getAllByText(/^Game Changers$/i).length).toBeGreaterThan(0);
    expect(screen.getAllByText("Smothering Tithe").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Cyclonic Rift").length).toBeGreaterThan(0);
    expect(screen.getByText(/2025-09-24-wotc/)).toBeInTheDocument();
  });

  it("fires onCardClick when a contributing card is clicked", () => {
    const onCardClick = vi.fn();
    render(<BracketAuditPanel estimate={estimate} manualBracket={null} onCardClick={onCardClick} />);
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    fireEvent.click(screen.getAllByText("Smothering Tithe")[0]);
    expect(onCardClick).toHaveBeenCalledWith("Smothering Tithe");
  });

  it("renders a check row for a rule that did not fire", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={null} onCardClick={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(screen.getByText(/0 of 1/i)).toBeInTheDocument();
  });

  it("shows how many more cards cross the next floor", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={null} onCardClick={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(screen.getByText(/2 more crosses to B4/i)).toBeInTheDocument();
  });

  it("renders the official line and its dated source", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={null} onCardClick={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(
      screen.getByText("you should not expect to see these cards anywhere in Brackets 1-3"),
    ).toBeInTheDocument();
    expect(screen.getAllByText(/2025-02-11/).length).toBeGreaterThan(0);
  });

  it("shows the unresolved list when confidence is partial", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={null} onCardClick={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(screen.getByText("Not found in card data")).toBeInTheDocument();
    expect(screen.getByText("Missing Card")).toBeInTheDocument();
  });

  it("renders no unresolved block when complete", () => {
    const completeEstimate: BracketEstimate = {
      ...estimate,
      coverage: { counted: 8, resolved: 8, unresolved: [], confidence: "complete" },
    };
    render(
      <BracketAuditPanel
        estimate={completeEstimate}
        manualBracket={null}
        onCardClick={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(screen.queryByText("Not found in card data")).toBeNull();
  });

  it("renders an empty-state placeholder when estimate is null and format is Commander", () => {
    render(
      <BracketAuditPanel
        estimate={null}
        manualBracket={null}
        onCardClick={() => {}}
        emptyReason="no-commander"
      />,
    );
    expect(screen.getByText(/Add a commander/i)).toBeInTheDocument();
  });

  it("renders nothing for non-Commander formats", () => {
    const { container } = render(
      <BracketAuditPanel
        estimate={null}
        manualBracket={null}
        onCardClick={() => {}}
        emptyReason="not-commander"
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});
