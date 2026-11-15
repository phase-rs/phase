import { create } from "zustand";

/**
 * Why boot left audio switched off. Each variant is a distinct system fault
 * with its own remedy, so the store carries the reason rather than a bare
 * "blocked" flag — the UI picks its message from the reason.
 *
 * - `device-wedged`: the shell's boot probe found the OS audio server hanging
 *   on stream open, so the device was never opened at all. See
 *   `services/audioHealth.ts`.
 * - `media-unavailable`: the device opened, but the platform's media pipeline
 *   never finished decoding a sound within the boot deadline. On Linux this is
 *   WebKitGTK with no usable GStreamer plugin set; the shell prints which
 *   plugins are missing (`src-tauri/src/media_stack.rs`).
 */
export type AudioUnavailableReason = "device-wedged" | "media-unavailable";

interface AudioHealthState {
  /** `null` while audio is healthy. */
  unavailable: AudioUnavailableReason | null;
  setUnavailable: (unavailable: AudioUnavailableReason | null) => void;
}

export const useAudioHealthStore = create<AudioHealthState>((set) => ({
  unavailable: null,
  setUnavailable: (unavailable) => set({ unavailable }),
}));
