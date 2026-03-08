import { create } from "zustand";
import type { GameEvent } from "../adapter/types";

export interface AnimationEffect {
  type: string;
  data: unknown;
  duration: number;
}

interface AnimationStoreState {
  queue: AnimationEffect[];
  isPlaying: boolean;
  positionRegistry: Map<number, DOMRect>;
}

interface AnimationStoreActions {
  enqueueEffects: (events: GameEvent[]) => void;
  playNext: () => AnimationEffect | undefined;
  registerPosition: (objectId: number, rect: DOMRect) => void;
  clearQueue: () => void;
}

export type AnimationStore = AnimationStoreState & AnimationStoreActions;

const EVENT_DURATIONS: Record<string, number> = {
  ZoneChanged: 400,
  DamageDealt: 300,
  LifeChanged: 300,
  SpellCast: 500,
  CreatureDestroyed: 400,
  TokenCreated: 400,
  CounterAdded: 200,
  CounterRemoved: 200,
  PermanentTapped: 200,
  PermanentUntapped: 200,
  AttackersDeclared: 300,
  BlockersDeclared: 300,
};

const DEFAULT_DURATION = 200;

export const useAnimationStore = create<AnimationStore>()((set, get) => ({
  queue: [],
  isPlaying: false,
  positionRegistry: new Map(),

  enqueueEffects: (events) => {
    const effects: AnimationEffect[] = events.map((event) => ({
      type: event.type,
      data: "data" in event ? event.data : undefined,
      duration: EVENT_DURATIONS[event.type] ?? DEFAULT_DURATION,
    }));

    set((state) => ({
      queue: [...state.queue, ...effects],
      isPlaying: true,
    }));
  },

  playNext: () => {
    const { queue } = get();
    if (queue.length === 0) {
      set({ isPlaying: false });
      return undefined;
    }

    const [next, ...rest] = queue;
    set({ queue: rest, isPlaying: rest.length > 0 });
    return next;
  },

  registerPosition: (objectId, rect) => {
    set((state) => {
      const newRegistry = new Map(state.positionRegistry);
      newRegistry.set(objectId, rect);
      return { positionRegistry: newRegistry };
    });
  },

  clearQueue: () => set({ queue: [], isPlaying: false }),
}));
