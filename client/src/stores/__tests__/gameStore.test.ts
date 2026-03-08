import { describe, it } from 'vitest';

describe('gameStore', () => {
  it.todo('initializes with null gameState');
  it.todo('initGame sets adapter and creates initial game state');
  it.todo('dispatch calls adapter.submitAction and updates state');
  it.todo('dispatch pushes to stateHistory for unrevealed-info actions');
  it.todo('undo restores previous state from stateHistory');
  it.todo('undo is unavailable when stateHistory is empty');
  it.todo('reset clears all state');
});
