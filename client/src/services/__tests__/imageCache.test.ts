import { describe, it } from 'vitest';

describe('imageCache', () => {
  it.todo('getCachedImage returns null on cache miss');
  it.todo('cacheImage stores blob and getCachedImage retrieves it');
  it.todo('getCachedImage returns object URL from cached blob');
  it.todo('revokeImageUrl calls URL.revokeObjectURL');
});
