import { describe, it, expect, vi } from 'vitest';
import {
  parseDeckFile,
  exportDeckFile,
  parseMtgaDeck,
  detectAndParseDeck,
  repairParsedDeck,
  resolveCommander,
  expandParsedDeck,
} from '../deckParser';

vi.mock('../engineRuntime', () => ({
  isCardCommanderEligible: vi.fn(),
}));

describe('deckParser', () => {
  it('parses simple deck: "4 Lightning Bolt" -> { count: 4, name: "Lightning Bolt" }', () => {
    const result = parseDeckFile('4 Lightning Bolt');
    expect(result.main).toEqual([{ count: 4, name: 'Lightning Bolt' }]);
    expect(result.sideboard).toEqual([]);
  });

  it('parses "4x Lightning Bolt" format', () => {
    const result = parseDeckFile('4x Lightning Bolt');
    expect(result.main).toEqual([{ count: 4, name: 'Lightning Bolt' }]);
  });

  it('parses [Main] and [Sideboard] sections', () => {
    const content = `[Main]
4 Lightning Bolt
2 Mountain
[Sideboard]
3 Red Elemental Blast`;
    const result = parseDeckFile(content);
    expect(result.main).toEqual([
      { count: 4, name: 'Lightning Bolt' },
      { count: 2, name: 'Mountain' },
    ]);
    expect(result.sideboard).toEqual([
      { count: 3, name: 'Red Elemental Blast' },
    ]);
  });

  it('skips comment lines starting with #', () => {
    const content = `# This is a comment
4 Lightning Bolt
# Another comment
2 Mountain`;
    const result = parseDeckFile(content);
    expect(result.main).toHaveLength(2);
  });

  it('skips empty lines', () => {
    const content = `4 Lightning Bolt

2 Mountain

`;
    const result = parseDeckFile(content);
    expect(result.main).toHaveLength(2);
  });

  it('exportDeckFile produces valid .dck format', () => {
    const deck = {
      main: [
        { count: 4, name: 'Lightning Bolt' },
        { count: 2, name: 'Mountain' },
      ],
      sideboard: [{ count: 3, name: 'Red Elemental Blast' }],
    };
    const output = exportDeckFile(deck);
    expect(output).toBe(
      '[Main]\n4 Lightning Bolt\n2 Mountain\n[Sideboard]\n3 Red Elemental Blast\n',
    );
  });

  it('round-trips: parse then export produces equivalent deck', () => {
    const original = `[Main]
4 Lightning Bolt
2 Mountain
[Sideboard]
3 Red Elemental Blast
`;
    const deck = parseDeckFile(original);
    const exported = exportDeckFile(deck);
    const reparsed = parseDeckFile(exported);
    expect(reparsed).toEqual(deck);
  });

  it('handles case-insensitive section headers', () => {
    const content = `[MAIN]
4 Lightning Bolt
[SIDEBOARD]
2 Pyroblast`;
    const result = parseDeckFile(content);
    expect(result.main).toHaveLength(1);
    expect(result.sideboard).toHaveLength(1);
  });

  it('defaults to main section when no header present', () => {
    const content = `4 Lightning Bolt
2 Mountain`;
    const result = parseDeckFile(content);
    expect(result.main).toHaveLength(2);
    expect(result.sideboard).toEqual([]);
  });

  it('sets companion field without adding to sideboard', () => {
    const content = `[Companion]
1 Lurrus of the Dream-Den
[Main]
4 Lightning Bolt`;
    const result = parseDeckFile(content);
    expect(result.companion).toBe('Lurrus of the Dream-Den');
    // Companion name recorded; sideboard entry comes from [Sideboard] section
    expect(result.sideboard).toEqual([]);
    expect(result.main).toHaveLength(1);
  });
});

describe('parseMtgaDeck', () => {
  it('parses MTGA format lines into main deck', () => {
    const content = '4 Lightning Bolt (FDN) 123\n2 Counterspell (MKM) 56';
    const result = parseMtgaDeck(content);
    expect(result.main).toEqual([
      { count: 4, name: 'Lightning Bolt' },
      { count: 2, name: 'Counterspell' },
    ]);
    expect(result.sideboard).toEqual([]);
  });

  it('puts cards after blank line into sideboard', () => {
    const content = `4 Lightning Bolt (FDN) 123
2 Mountain (FDN) 280

3 Red Elemental Blast (LEA) 166`;
    const result = parseMtgaDeck(content);
    expect(result.main).toHaveLength(2);
    expect(result.sideboard).toEqual([
      { count: 3, name: 'Red Elemental Blast' },
    ]);
  });

  it('ignores empty lines at start/end and comment lines', () => {
    const content = `
# This is a comment
4 Lightning Bolt (FDN) 123
# Another comment
2 Mountain (FDN) 280
`;
    const result = parseMtgaDeck(content);
    expect(result.main).toHaveLength(2);
  });

  it('handles "Deck" and "Sideboard" header labels', () => {
    const content = `Deck
4 Lightning Bolt (FDN) 123
2 Mountain (FDN) 280
Sideboard
3 Red Elemental Blast (LEA) 166`;
    const result = parseMtgaDeck(content);
    expect(result.main).toEqual([
      { count: 4, name: 'Lightning Bolt' },
      { count: 2, name: 'Mountain' },
    ]);
    expect(result.sideboard).toEqual([
      { count: 3, name: 'Red Elemental Blast' },
    ]);
  });

  it('handles MTGA-style Deck and Sideboard headers with simple card lines', () => {
    const content = `Deck
4 Lightning Bolt
2 Mountain
Sideboard
3 Red Elemental Blast`;
    const result = detectAndParseDeck(content);
    expect(result.main).toEqual([
      { count: 4, name: 'Lightning Bolt' },
      { count: 2, name: 'Mountain' },
    ]);
    expect(result.sideboard).toEqual([
      { count: 3, name: 'Red Elemental Blast' },
    ]);
  });

  it('handles "Companion" header label', () => {
    const content = `Companion
1 Lurrus of the Dream-Den (IKO) 226

Deck
4 Lightning Bolt (FDN) 123`;
    const result = parseMtgaDeck(content);
    // Companion name recorded; sideboard entry comes from Sideboard section
    expect(result.main).toHaveLength(1);
    expect(result.companion).toBe('Lurrus of the Dream-Den');
    expect(result.sideboard).toEqual([]);
  });

  it('handles multi-word card names with special characters', () => {
    const content = "2 Lim-Dul's Vault (ALL) 107";
    const result = parseMtgaDeck(content);
    expect(result.main).toEqual([
      { count: 2, name: "Lim-Dul's Vault" },
    ]);
  });

  it('promotes a trailing singleton sideboard card to commander for commander-shaped imports', () => {
    const content = `1 Sol Ring
90 Swamp
8 Plains

1 Dark Leo & Shredder`;
    const result = parseMtgaDeck(content);
    expect(result.main).toEqual([
      { count: 1, name: 'Sol Ring' },
      { count: 90, name: 'Swamp' },
      { count: 8, name: 'Plains' },
    ]);
    expect(result.sideboard).toEqual([]);
    expect(result.commander).toEqual(['Dark Leo & Shredder']);
  });

  it('normalizes split-card names during import', () => {
    const result = parseMtgaDeck('1 Revival/Revenge');
    expect(result.main).toEqual([
      { count: 1, name: 'Revival // Revenge' },
    ]);
  });

  it('preserves an explicit sideboard header instead of promoting commander heuristically', () => {
    const content = `Deck
1 Sol Ring
90 Swamp
8 Plains
Sideboard
1 Dark Leo & Shredder`;
    const result = parseMtgaDeck(content);
    expect(result.commander).toBeUndefined();
    expect(result.sideboard).toEqual([
      { count: 1, name: 'Dark Leo & Shredder' },
    ]);
  });
});

describe('detectAndParseDeck', () => {
  it('auto-detects MTGA format and parses correctly', () => {
    const mtgaContent = '4 Lightning Bolt (FDN) 123\n2 Counterspell (MKM) 56';
    const result = detectAndParseDeck(mtgaContent);
    expect(result.main).toEqual([
      { count: 4, name: 'Lightning Bolt' },
      { count: 2, name: 'Counterspell' },
    ]);
  });

  it('auto-detects .dck format and parses correctly', () => {
    const dckContent = `[Main]
4 Lightning Bolt
2 Mountain`;
    const result = detectAndParseDeck(dckContent);
    expect(result.main).toEqual([
      { count: 4, name: 'Lightning Bolt' },
      { count: 2, name: 'Mountain' },
    ]);
  });

  it('detects plain "count CardName" as .dck format', () => {
    const plainContent = '4 Lightning Bolt\n2 Mountain';
    const result = detectAndParseDeck(plainContent);
    expect(result.main).toEqual([
      { count: 4, name: 'Lightning Bolt' },
      { count: 2, name: 'Mountain' },
    ]);
  });

  it('detects simple Deck/Sideboard sections and preserves sideboard cards', () => {
    const content = `Deck
4 Lightning Bolt

Sideboard
3 Red Elemental Blast`;
    const result = detectAndParseDeck(content);
    expect(result.main).toEqual([
      { count: 4, name: 'Lightning Bolt' },
    ]);
    expect(result.sideboard).toEqual([
      { count: 3, name: 'Red Elemental Blast' },
    ]);
  });

  it('parses MTGA lines with empty set codes (Archidekt Three Visits export)', () => {
    const content = '1 Three Visits () 315';
    const result = detectAndParseDeck(content);
    expect(result.main).toEqual([{ count: 1, name: 'Three Visits' }]);
  });

  it('strips nonnumeric collector-number suffixes from MTGA-style lines', () => {
    const content = [
      '1 Arcane Signet () 1F★',
      '1 Arcane Denial () 22a',
      '1 Mental Misstep () 2023-1',
      '1 Narset, Parter of Veils (WAR) 61★',
    ].join('\n');

    const result = detectAndParseDeck(content);
    expect(result.main).toEqual([
      { count: 1, name: 'Arcane Signet' },
      { count: 1, name: 'Arcane Denial' },
      { count: 1, name: 'Mental Misstep' },
      { count: 1, name: 'Narset, Parter of Veils' },
    ]);
  });

  it('routes inline [Commander] annotations to the commander slot', () => {
    const content = `1 Zimone, Infinite Analyst (SOC) 10 [Commander {top}]
1 Sol Ring (SOC) 128`;
    const result = detectAndParseDeck(content);
    expect(result.commander).toEqual(['Zimone, Infinite Analyst']);
    expect(result.main).toEqual([{ count: 1, name: 'Sol Ring' }]);
  });

  it('routes inline *CMDR* annotations on simple lines to the commander slot', () => {
    const content = `1 Atraxa, Praetors' Voice *CMDR*
1 Sol Ring`;
    const result = detectAndParseDeck(content);
    expect(result.commander).toEqual(["Atraxa, Praetors' Voice"]);
    expect(result.main).toEqual([{ count: 1, name: 'Sol Ring' }]);
  });

  it('routes inline [Companion] annotation to the companion field', () => {
    const content = `1 Lurrus of the Dream-Den (IKO) 226 [Companion]
1 Sol Ring (SOC) 128`;
    const result = detectAndParseDeck(content);
    expect(result.companion).toBe('Lurrus of the Dream-Den');
    expect(result.main).toEqual([{ count: 1, name: 'Sol Ring' }]);
  });

  it('recognizes "Commanders" section header (Archidekt categorized export)', () => {
    const content = `Commanders
1 Zimone, Infinite Analyst (SOC) 10

Deck
1 Sol Ring (SOC) 128`;
    const result = detectAndParseDeck(content);
    expect(result.commander).toEqual(['Zimone, Infinite Analyst']);
    expect(result.main).toEqual([{ count: 1, name: 'Sol Ring' }]);
  });

  it('removes one matching main-deck copy when a commander is explicit', () => {
    const content = `Commander
1 Zimone, Infinite Analyst
Deck
1 Zimone, Infinite Analyst
1 Sol Ring`;
    const result = detectAndParseDeck(content);
    expect(result.commander).toEqual(['Zimone, Infinite Analyst']);
    expect(result.main).toEqual([{ count: 1, name: 'Sol Ring' }]);
  });

  it('repairs saved decks that still have the commander in the main deck', () => {
    const result = repairParsedDeck({
      commander: ['Zimone, Infinite Analyst'],
      main: [
        { count: 1, name: 'Zimone, Infinite Analyst' },
        { count: 1, name: 'Sol Ring' },
      ],
      sideboard: [],
    });
    expect(result.commander).toEqual(['Zimone, Infinite Analyst']);
    expect(result.main).toEqual([{ count: 1, name: 'Sol Ring' }]);
  });
});

describe('resolveCommander waterfall', () => {
  it('promotes the first eligible card when the deck is 100 singletons', async () => {
    const { isCardCommanderEligible } = await import('../engineRuntime');
    vi.mocked(isCardCommanderEligible).mockImplementation((name) =>
      Promise.resolve(name === 'Zimone, Infinite Analyst')
    );

    const main = [
      { count: 1, name: 'Sol Ring' },
      { count: 1, name: 'Zimone, Infinite Analyst' },
      ...Array.from({ length: 86 }, (_, i) => ({ count: 1, name: `Card ${i}` })),
      { count: 7, name: 'Island' },
      { count: 5, name: 'Forest' },
    ];
    const resolved = await resolveCommander({ main, sideboard: [] });

    expect(resolved.commander).toEqual(['Zimone, Infinite Analyst']);
    expect(resolved.main[0].name).toBe('Sol Ring');
    expect(resolved.main).not.toContainEqual({ count: 1, name: 'Zimone, Infinite Analyst' });
    expect(resolved.main).toHaveLength(87 + 2);
    expect(isCardCommanderEligible).toHaveBeenCalledWith('Sol Ring');
    expect(isCardCommanderEligible).toHaveBeenCalledWith('Zimone, Infinite Analyst');
  });

  it('does not promote when the first card is not commander-eligible', async () => {
    const { isCardCommanderEligible } = await import('../engineRuntime');
    vi.mocked(isCardCommanderEligible).mockResolvedValue(false);

    const main = [
      { count: 1, name: 'Sol Ring' },
      ...Array.from({ length: 86 }, (_, i) => ({ count: 1, name: `Card ${i}` })),
      { count: 7, name: 'Island' },
      { count: 6, name: 'Forest' },
    ];
    const resolved = await resolveCommander({ main, sideboard: [] });

    expect(resolved.commander).toBeUndefined();
    expect(resolved.main).toEqual(main);
  });

  it('skips lookup when the deck already has a commander', async () => {
    const { isCardCommanderEligible } = await import('../engineRuntime');
    vi.mocked(isCardCommanderEligible).mockReset();

    const resolved = await resolveCommander({
      main: [{ count: 1, name: 'Sol Ring' }],
      sideboard: [],
      commander: ['Zimone, Infinite Analyst'],
    });

    expect(resolved.commander).toEqual(['Zimone, Infinite Analyst']);
    expect(isCardCommanderEligible).not.toHaveBeenCalled();
  });

  it('end-to-end: handles an unmarked Archidekt-style 100-card paste with empty set codes', async () => {
    const { isCardCommanderEligible } = await import('../engineRuntime');
    vi.mocked(isCardCommanderEligible).mockResolvedValue(true);

    // 87 singletons + 7 Island + 6 Forest = 100 cards, no commander/sideboard headers.
    // Includes "Three Visits () 315" with an empty set code (Archidekt edge case).
    const singletons = Array.from({ length: 86 }, (_, i) =>
      i === 30 ? '1 Three Visits () 315' : `1 Generic Card ${i} (SOC) ${i + 1}`
    );
    const content = [
      '1 Zimone, Infinite Analyst (SOC) 10',
      ...singletons,
      '7 Island (SOS) 274',
      '6 Forest (SOS) 280',
    ].join('\n');

    const parsed = detectAndParseDeck(content);
    expect(parsed.commander).toBeUndefined();
    expect(parsed.main).toContainEqual({ count: 1, name: 'Three Visits' });
    expect(parsed.main[0]).toEqual({ count: 1, name: 'Zimone, Infinite Analyst' });

    const resolved = await resolveCommander(parsed);
    expect(resolved.commander).toEqual(['Zimone, Infinite Analyst']);
    expect(resolved.main).not.toContainEqual({ count: 1, name: 'Zimone, Infinite Analyst' });
    expect(resolved.main.reduce((s, e) => s + e.count, 0)).toBe(99);
  });

  it('skips lookup when the deck is not 100 cards', async () => {
    const { isCardCommanderEligible } = await import('../engineRuntime');
    vi.mocked(isCardCommanderEligible).mockReset();

    const resolved = await resolveCommander({
      main: [{ count: 60, name: 'Mountain' }],
      sideboard: [],
    });

    expect(resolved.commander).toBeUndefined();
    expect(isCardCommanderEligible).not.toHaveBeenCalled();
  });
});

describe('expandParsedDeck', () => {
  it('expands count-grouped entries into a flat name list', () => {
    const result = expandParsedDeck({
      main: [
        { count: 4, name: 'Lightning Bolt' },
        { count: 2, name: 'Mountain' },
      ],
      sideboard: [{ count: 3, name: 'Pyroblast' }],
    });
    expect(result.main_deck).toEqual([
      'Lightning Bolt',
      'Lightning Bolt',
      'Lightning Bolt',
      'Lightning Bolt',
      'Mountain',
      'Mountain',
    ]);
    expect(result.sideboard).toEqual(['Pyroblast', 'Pyroblast', 'Pyroblast']);
  });

  it('preserves the commander slot when present (regression: host Start-Game deck-invalid)', () => {
    const result = expandParsedDeck({
      main: [{ count: 99, name: 'Island' }],
      sideboard: [],
      commander: ['Kenrith, the Returned King'],
    });
    expect(result.commander).toEqual(['Kenrith, the Returned King']);
  });

  it('defaults commander to an empty array when absent', () => {
    const result = expandParsedDeck({
      main: [{ count: 60, name: 'Swamp' }],
      sideboard: [],
    });
    expect(result.commander).toEqual([]);
  });
});
