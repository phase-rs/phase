import type { DeckEntry } from "../services/deckParser";

export interface StarterDeck {
  name: string;
  colorIdentity: string[];
  cards: DeckEntry[];
}

export const STARTER_DECKS: StarterDeck[] = [
  {
    name: "Red Deck Wins",
    colorIdentity: ["R"],
    cards: [
      { count: 4, name: "Lightning Bolt" },
      { count: 4, name: "Shock" },
      { count: 4, name: "Goblin Guide" },
      { count: 4, name: "Monastery Swiftspear" },
      { count: 4, name: "Jackal Pup" },
      { count: 4, name: "Raging Goblin" },
      { count: 4, name: "Searing Spear" },
      { count: 4, name: "Volcanic Hammer" },
      { count: 4, name: "Firebolt" },
      { count: 24, name: "Mountain" },
    ],
  },
  {
    name: "White Weenie",
    colorIdentity: ["W"],
    cards: [
      { count: 4, name: "Savannah Lions" },
      { count: 4, name: "Elite Vanguard" },
      { count: 4, name: "Raise the Alarm" },
      { count: 4, name: "Precinct Captain" },
      { count: 4, name: "Swords to Plowshares" },
      { count: 4, name: "Glorious Anthem" },
      { count: 4, name: "Benalish Marshal" },
      { count: 4, name: "Soldier of the Pantheon" },
      { count: 4, name: "Honor of the Pure" },
      { count: 24, name: "Plains" },
    ],
  },
  {
    name: "Blue Control",
    colorIdentity: ["U"],
    cards: [
      { count: 4, name: "Counterspell" },
      { count: 4, name: "Cancel" },
      { count: 4, name: "Air Elemental" },
      { count: 4, name: "Unsummon" },
      { count: 4, name: "Divination" },
      { count: 4, name: "Essence Scatter" },
      { count: 2, name: "Wind Drake" },
      { count: 4, name: "Opt" },
      { count: 4, name: "Negate" },
      { count: 26, name: "Island" },
    ],
  },
  {
    name: "Green Stompy",
    colorIdentity: ["G"],
    cards: [
      { count: 4, name: "Llanowar Elves" },
      { count: 4, name: "Giant Growth" },
      { count: 4, name: "Grizzly Bears" },
      { count: 4, name: "Elvish Mystic" },
      { count: 4, name: "Leatherback Baloth" },
      { count: 4, name: "Rancor" },
      { count: 4, name: "Garruk's Companion" },
      { count: 4, name: "Kalonian Tusker" },
      { count: 4, name: "Giant Spider" },
      { count: 24, name: "Forest" },
    ],
  },
  {
    name: "Azorius Flyers",
    colorIdentity: ["W", "U"],
    cards: [
      { count: 4, name: "Suntail Hawk" },
      { count: 4, name: "Wind Drake" },
      { count: 4, name: "Serra Angel" },
      { count: 4, name: "Favorable Winds" },
      { count: 4, name: "Swords to Plowshares" },
      { count: 4, name: "Counterspell" },
      { count: 4, name: "Opt" },
      { count: 4, name: "Warden of Evos Isle" },
      { count: 4, name: "Unsummon" },
      { count: 12, name: "Plains" },
      { count: 12, name: "Island" },
    ],
  },
];
