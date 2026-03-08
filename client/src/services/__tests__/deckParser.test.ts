import { describe, it, expect } from 'vitest';
import { parseDeckFile, exportDeckFile } from '../deckParser';

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
});
