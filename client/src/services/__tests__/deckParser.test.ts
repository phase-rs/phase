import { describe, it, expect } from 'vitest';
import {
  parseDeckFile,
  exportDeckFile,
  parseMtgaDeck,
  detectAndParseDeck,
} from '../deckParser';

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
});
