import { create } from "zustand";

import type {
  ContestClickInput,
  ContestKind,
  ContestResult,
  PlayerId,
} from "../adapter/types.ts";

/** How long the result banner stays up after the engine settles a contest. */
export const CONTEST_RESULT_BANNER_MS = 4000;

/** Most clicks the local host queue keeps while the engine is unreachable; the oldest
 *  go first, since only recent activity can still matter to a 2s inactivity window. */
const MAX_BUFFERED_CLICKS = 256;

/** The most recent settled contest, kept after the engine has moved on so the
 *  result can be announced to both players. Display data only. */
export interface ContestAnnouncement {
  kind: ContestKind;
  participants: [PlayerId, PlayerId];
  result: ContestResult;
  /** Monotonic announcement id, so a repeat of an identical result re-shows. */
  id: number;
}

interface ContestStoreState {
  /** Live click counters pushed between full state snapshots, by seat, and the
   *  contest they belong to — counters for any other contest are never shown. */
  progress: { contest: number | null; clicks: Record<number, number> };
  announcement: ContestAnnouncement | null;
  nextAnnouncementId: number;
  /** Clicks awaiting the next local-host tick (local engines only). */
  pendingClicks: ContestClickInput[];
  bufferClick: (click: ContestClickInput) => void;
  /** Take every buffered click, oldest first. */
  drainClicks: () => ContestClickInput[];
  /** Put back clicks a host tick took but the engine rejected before accepting them,
   *  ahead of any newer ones. The engine's sequence check keeps a retry from counting
   *  a click it did accept; the bound keeps a failing host from growing the queue. */
  restoreClicks: (clicks: readonly ContestClickInput[]) => void;
  setProgress: (contest: number, clicks: ReadonlyArray<readonly [PlayerId, number]>) => void;
  announce: (kind: ContestKind, participants: [PlayerId, PlayerId], result: ContestResult) => void;
  dismissAnnouncement: (id: number) => void;
  reset: () => void;
}

const NO_PROGRESS = { contest: null, clicks: {} };
const initial = {
  progress: NO_PROGRESS,
  announcement: null,
  nextAnnouncementId: 1,
  pendingClicks: [],
};

export const useContestStore = create<ContestStoreState>((set, get) => ({
  ...initial,
  bufferClick: (click) => set((state) => ({ pendingClicks: [...state.pendingClicks, click] })),
  drainClicks: () => {
    const clicks = get().pendingClicks;
    if (clicks.length > 0) set({ pendingClicks: [] });
    return clicks;
  },
  restoreClicks: (clicks) =>
    set((state) => ({
      pendingClicks: [...clicks, ...state.pendingClicks].slice(-MAX_BUFFERED_CLICKS),
    })),
  setProgress: (contest, clicks) =>
    set({
      progress: {
        contest,
        clicks: Object.fromEntries(clicks.map(([player, count]) => [player, count])),
      },
    }),
  announce: (kind, participants, result) =>
    set((state) => ({
      announcement: { kind, participants, result, id: state.nextAnnouncementId },
      nextAnnouncementId: state.nextAnnouncementId + 1,
      progress: NO_PROGRESS,
    })),
  dismissAnnouncement: (id) =>
    set((state) => (state.announcement?.id === id ? { announcement: null } : state)),
  reset: () => set({ ...initial }),
}));
