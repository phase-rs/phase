import { create } from "zustand";

interface AudioHealthState {
  /** True when boot skipped audio because the OS audio server was wedged. */
  deviceBlocked: boolean;
  setDeviceBlocked: (deviceBlocked: boolean) => void;
}

export const useAudioHealthStore = create<AudioHealthState>((set) => ({
  deviceBlocked: false,
  setDeviceBlocked: (deviceBlocked) => set({ deviceBlocked }),
}));
