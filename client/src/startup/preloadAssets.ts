import { audioManager, initAudioOnInteraction, type SfxPreloadResult } from "../audio/AudioManager";
import { audioDeviceSafe } from "../services/audioHealth";
import { useAudioHealthStore } from "../stores/audioHealthStore";

export interface PreloadProgress {
  phase: "audio" | "complete";
  percent: number;
}

type ProgressListener = (progress: PreloadProgress) => void;

const listeners = new Set<ProgressListener>();
let preloadPromise: Promise<void> | null = null;

/**
 * How long the splash waits for SFX buffers before booting without them.
 *
 * Decoding the handful of short local files the default theme ships is
 * milliseconds of work on a healthy system, so this only ever fires on a
 * broken one — where it must fire, because `decodeAudioData` is not
 * guaranteed to settle at all. WebKitGTK assembles every decode as a
 * GStreamer pipeline; when the plugins it needs are missing it logs the
 * missing elements, wires up a partial pipeline, and leaves the promise
 * pending forever (issue #6744). Audio is optional, boot is not.
 */
export const SFX_PRELOAD_DEADLINE_MS = 8000;

/** How the bounded audio phase ended. */
type SfxPreloadOutcome = "ready" | "failed" | "timed-out";

function emit(progress: PreloadProgress): void {
  for (const listener of listeners) {
    listener(progress);
  }
}

/** Subscribe to preload progress updates. Returns an unsubscribe function. */
export function subscribePreload(listener: ProgressListener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/**
 * Await the SFX preload without letting it own the boot.
 *
 * Mirrors the verdict race in `services/audioHealth.ts`: the deadline resolves
 * a sentinel rather than rejecting, because a deadline that fires is an
 * expected boot outcome on a broken host, not an error to handle. A rejection
 * from the preload itself is the same kind of outcome and is reported as one.
 *
 * Fulfilment alone is not success. `loadBuffer` swallows per-file failures, so
 * a host whose decoder rejects every file still fulfils here — which is why
 * the preload reports what it loaded and `"none"` is classified as a failure
 * rather than read as readiness.
 *
 * `work` is left running on a timeout — its buffers are still welcome if they
 * ever land, and a pending `decodeAudioData` cannot be cancelled.
 */
function awaitSfxPreload(
  work: Promise<SfxPreloadResult>,
  limitMs: number,
): Promise<SfxPreloadOutcome> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  return Promise.race<SfxPreloadOutcome>([
    work.then(
      (result) => (result === "none" ? "failed" : "ready"),
      () => "failed",
    ),
    new Promise<SfxPreloadOutcome>((resolve) => {
      timer = setTimeout(() => resolve("timed-out"), limitMs);
    }),
  ]).finally(() => clearTimeout(timer));
}

/**
 * Actionable line for the terminal, per outcome. Only called once boot has
 * concluded the host cannot play sound at all.
 *
 * Not routed through `t()` on purpose: `i18n/README.md` puts console output on
 * the never-translate list, and this line's counterpart is the shell's English
 * stderr from `src-tauri/src/media_stack.rs` — the two are read together in one
 * terminal. The user-facing half of this state is localized, in the volume
 * control's `volume.mediaUnavailable`.
 */
function sfxFailureDiagnostic(outcome: Exclude<SfxPreloadOutcome, "ready">): string {
  const cause =
    outcome === "timed-out"
      ? `no sound finished decoding within ${SFX_PRELOAD_DEADLINE_MS}ms`
      : "no sound could be decoded";
  return (
    `[audio] Starting without audio: ${cause}. This platform's media pipeline cannot ` +
    "play sound. On Linux that means WebKit found no usable GStreamer plugins — run " +
    "the app from a terminal, where the shell lists exactly which plugins are missing " +
    "and the packages that provide them."
  );
}

/**
 * Run the startup preload sequence:
 * 1. Register music interaction listeners
 * 2. Preload SFX audio buffers
 *
 * Also registers audio interaction listeners for music playback.
 * Idempotent — safe to call multiple times.
 *
 * Every audio step is bounded or skippable. The splash this drives covers the
 * whole app, so no audio fault may leave this promise unresolved.
 */
export function ensurePreload(): Promise<void> {
  if (preloadPromise) return preloadPromise;

  preloadPromise = (async () => {
    emit({ phase: "audio", percent: 10 });

    // Ask the shell whether the OS audio device is safe to open BEFORE any
    // audio wiring: on a wedged audio server, WebKitGTK's synchronous device
    // open inside `new AudioContext()` would freeze the whole page (see
    // services/audioHealth.ts). Gesture listeners are registered only after
    // the verdict so a click cannot reach warmUp() early.
    if (!(await audioDeviceSafe())) {
      audioManager.disable();
      useAudioHealthStore.getState().setUnavailable("device-wedged");
    }
    audioManager.armDeviceOpen();
    initAudioOnInteraction();

    emit({ phase: "audio", percent: 20 });
    audioManager.warmUp();
    const outcome = await awaitSfxPreload(audioManager.preloadSfx(), SFX_PRELOAD_DEADLINE_MS);
    if (outcome === "ready") {
      // Nothing to do — every file settled and at least one decoded.
    } else if (outcome === "timed-out" && audioManager.sfxAvailability() !== "none") {
      // A fired deadline is not a verdict on its own. `preloadSfx` cannot
      // resolve while a single file hangs, but its siblings' buffers are
      // already in the map and playable — so ask what landed before switching
      // anything off. Losing every sound to one bad file would be a worse bug
      // than the one this deadline exists to fix.
      console.warn(
        `[audio] Some sounds did not decode within ${SFX_PRELOAD_DEADLINE_MS}ms. ` +
          "Continuing with the ones that did.",
      );
    } else {
      // Nothing decoded, or the preload failed outright: this host cannot play
      // sound. Latch audio off so no later pipeline — theme loads, music
      // tracks, a gesture handler's ensurePlayback() — hangs or fails the same
      // way.
      audioManager.disable();
      useAudioHealthStore.getState().setUnavailable("media-unavailable");
      console.error(sfxFailureDiagnostic(outcome));
    }
    emit({ phase: "complete", percent: 100 });
  })();

  return preloadPromise;
}
