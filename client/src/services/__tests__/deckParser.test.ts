import { describe, it } from 'vitest';

describe('deckParser', () => {
  it.todo('parses simple deck: "4 Lightning Bolt" -> { count: 4, name: "Lightning Bolt" }');
  it.todo('parses "4x Lightning Bolt" format');
  it.todo('parses [Main] and [Sideboard] sections');
  it.todo('skips comment lines starting with #');
  it.todo('skips empty lines');
  it.todo('exportDeckFile produces valid .dck format');
});
