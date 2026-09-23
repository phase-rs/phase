import { cleanup, render, screen } from "@testing-library/react";
import { createInstance } from "i18next";
import { afterEach, describe, expect, it, vi } from "vitest";

import { BetweenGamesSideboardModal } from "../../multiplayer/BetweenGamesSideboardModal";
import { CommanderPanel } from "../CommanderPanel";
import { formatMetadata } from "../../../data/formatRegistry";
import type { ScryfallCard } from "../../../services/scryfall";
import plDeckBuilder from "../../../i18n/locales/pl/deck-builder.json";
import enDeckBuilder from "../../../i18n/locales/en/deck-builder.json";

afterEach(cleanup);

function entry(name: string, count: number) {
  return { card: { name }, count };
}

// Same shape as BetweenGamesSideboardModal.test.tsx's basePool — a 17-card main.
const basePool = {
  registered_main: [
    entry("Lightning Bolt", 4),
    entry("Counterspell", 3),
    entry("Mountain", 10),
  ],
  registered_sideboard: [entry("Pyroblast", 2), entry("Chalice", 1)],
  current_main: [
    entry("Lightning Bolt", 4),
    entry("Counterspell", 3),
    entry("Mountain", 10),
  ],
  current_sideboard: [entry("Pyroblast", 2), entry("Chalice", 1)],
};

const score = { p0_wins: 1, p1_wins: 0, draws: 0 };

function makeLegendaryCreature(name: string): ScryfallCard {
  return {
    name,
    mana_cost: "",
    cmc: 3,
    type_line: "Legendary Creature — Ninja",
    color_identity: ["B"],
    legalities: { commander: "legal" },
  };
}

describe("deck-size floor wording — neither component receives a format", () => {
  it("modal: a zero floor is not worded as a minimum; a non-zero one still is", () => {
    const { unmount } = render(
      <BetweenGamesSideboardModal
        pool={basePool}
        gameNumber={2}
        score={score}
        minMainDeckSize={0}
        maxSideboardSize={15}
        onSubmit={vi.fn()}
      />,
    );
    expect(screen.getByText("Main (17)")).toBeInTheDocument();
    expect(screen.queryByText(/min/)).toBeNull();
    unmount();

    render(
      <BetweenGamesSideboardModal
        pool={basePool}
        gameNumber={2}
        score={score}
        minMainDeckSize={17}
        maxSideboardSize={15}
        onSubmit={vi.fn()}
      />,
    );
    expect(screen.getByText("Main (17, min 17)")).toBeInTheDocument();
  });

  it("panel: an absent floor is not worded as an expected size, fed from the registry", () => {
    const name = "The Prismatic Piper";
    const sharedProps = {
      commanders: [name],
      deck: [{ name: "Filler", count: 10 }],
      deckComposition: "commanders-outside" as const,
      cardDataCache: new Map([[name, makeLegendaryCreature(name)]]),
      isCommanderEligible: () => true,
      onSetCommander: vi.fn(),
      onRemoveCommander: vi.fn(),
    };

    const ffc = render(
      <CommanderPanel
        {...sharedProps}
        deckSizeRule={formatMetadata("FreeformCommander")!.default_config.deck_size}
      />,
    );
    expect(screen.getByText("11 cards")).toBeInTheDocument();
    expect(screen.queryByText(/\//)).toBeNull();
    ffc.unmount();

    const commander = render(
      <CommanderPanel
        {...sharedProps}
        deckSizeRule={formatMetadata("Commander")!.default_config.deck_size}
      />,
    );
    expect(screen.getByText("11/100 cards")).toBeInTheDocument();
    commander.unmount();

    render(
      <CommanderPanel
        {...sharedProps}
        deckSizeRule={formatMetadata("CommanderDraft")!.default_config.deck_size}
      />,
    );
    expect(screen.getByText("11/60 cards")).toBeInTheDocument();
  });

  it("panel: a count of 1 takes the singular noun, not the denominator's plural", () => {
    const name = "The Prismatic Piper";
    render(
      <CommanderPanel
        commanders={[name]}
        deck={[]}
        deckComposition="commanders-outside"
        cardDataCache={new Map([[name, makeLegendaryCreature(name)]])}
        isCommanderEligible={() => true}
        onSetCommander={vi.fn()}
        onRemoveCommander={vi.fn()}
        deckSizeRule={formatMetadata("FreeformCommander")!.default_config.deck_size}
      />,
    );
    expect(screen.getByText("1 card")).toBeInTheDocument();
    expect(screen.queryByText("1 cards")).toBeNull();
  });

  // Same idiom as localeParity.test.ts's resolves_polish_one_few_many_and_other_without_fallback:
  // an isolated i18next instance resolving the real catalogues, so a missing _few/_many
  // form falls back to fallbackLng "en" exactly as the app's i18n instance is configured
  // (client/src/i18n/index.ts), reproducing the regression this pins against instead of
  // just asserting a string literal.
  it("panel: Polish few and many counts take Polish nouns, not the English fallback", async () => {
    const instance = createInstance();
    await instance.init({
      lng: "pl",
      fallbackLng: "en",
      resources: {
        pl: { "deck-builder": plDeckBuilder },
        en: { "deck-builder": enDeckBuilder },
      },
      interpolation: { escapeValue: false },
    });

    const few = instance.t("commanderPanel.cardCountNoMinimum", { ns: "deck-builder", count: 2 });
    const many = instance.t("commanderPanel.cardCountNoMinimum", { ns: "deck-builder", count: 5 });
    expect(few).toBe("2 karty");
    expect(many).toBe("5 kart");
    expect(few).not.toBe("2 cards");
    expect(many).not.toBe("5 cards");
  });
});
