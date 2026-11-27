import { vi } from "vitest";

const POINTER_KEY = "phase-active-draft-guest";
const POINTER_GENERATION_KEY = "phase-active-draft-pointer-generation";
const POINTER_RECORD_KEY = "phase-active-draft-pointer-record";

/**
 * Watches the reads of the recovery pointer that one pointer step's body
 * makes. Reads count from the moment the step's write-ahead of the pointer
 * generation, issued from now (and, with `after`, while it holds), has
 * committed in IndexedDB, so the barrier's own read before it is passed
 * over; the runner's read for the step's post-body record, which each write
 * of that record follows, is not counted either. `at.onRead` runs at the
 * `nth` counted read, which still returns what it read. It assumes one
 * pointer step at a time, as the FIFO lock double gives.
 */
export function watchPointerStepReads(at?: { nth: number; onRead: () => void; after?: () => boolean }): {
  reads: () => number;
  fired: () => number;
  restore: () => void;
} {
  let landed = false;
  let reads = 0;
  let fired = 0;
  const put = IDBObjectStore.prototype.put;
  const putSpy = vi.spyOn(IDBObjectStore.prototype, "put").mockImplementation(function (
    this: IDBObjectStore,
    value: unknown,
    key?: IDBValidKey,
  ) {
    const request = put.call(this, value, key);
    if (key === POINTER_GENERATION_KEY && !landed && (at?.after?.() ?? true)) {
      request.transaction?.addEventListener("complete", () => {
        landed = true;
      });
    } else if (key === POINTER_RECORD_KEY && landed) {
      reads -= 1;
    }
    return request;
  });
  const getItem = localStorage.getItem.bind(localStorage);
  const getItemSpy = vi.spyOn(localStorage, "getItem").mockImplementation((key: string) => {
    const seen = getItem(key);
    if (key === POINTER_KEY && landed) {
      reads += 1;
      if (at && fired === 0 && reads === at.nth) {
        fired += 1;
        at.onRead();
      }
    }
    return seen;
  });
  return {
    reads: () => reads,
    fired: () => fired,
    restore: () => {
      getItemSpy.mockRestore();
      putSpy.mockRestore();
    },
  };
}
