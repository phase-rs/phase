import { beforeEach, describe, expect, it, vi } from "vitest";

// Separate file from AudioManager.test.ts on purpose: `disable()` is
// deliberately sticky on the module singleton, so these tests get a fresh
// module per test (changelog.test.ts idiom) instead of sharing the suite's
// singleton and leaking the flag into its assertions.

const decodeAudioDataSpy = vi.fn().mockResolvedValue({});
const resumeSpy = vi.fn().mockResolvedValue(undefined);
const createMediaElementSourceSpy = vi
  .fn()
  .mockImplementation(() => ({ connect: vi.fn() }));
// Per-test so a fixture can put the LIVE context into the one state that
// reaches ensurePlayback's resume branch.
let contextState: AudioContextState = "running";

// `playTrack` hangs a continuation off `audio.play()`'s rejection. Holding the
// rejection lets a test latch `disable()` in the window between initiating
// playback and hearing back — the window the entry guard cannot see.
let rejectPlay: ((err: unknown) => void) | null = null;
const playSpy = vi.fn();

class StubAudio {
  crossOrigin = "";
  play = playSpy;
  pause = vi.fn();
  addEventListener = vi.fn();
  removeEventListener = vi.fn();
}
vi.stubGlobal("Audio", StubAudio);

/** Let queued microtasks (the catch, then its resume().then) run. */
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const createBufferSourceSpy = vi.fn().mockImplementation(() => ({
  buffer: null,
  connect: vi.fn(),
  start: vi.fn(),
}));

const audioContextSpy = vi.fn().mockImplementation(function () {
  return {
    createGain: vi.fn().mockImplementation(() => ({
      gain: {
        value: 1,
        cancelScheduledValues: vi.fn(),
        setValueAtTime: vi.fn(),
        linearRampToValueAtTime: vi.fn(),
      },
      connect: vi.fn(),
    })),
    createBufferSource: createBufferSourceSpy,
    createMediaElementSource: createMediaElementSourceSpy,
    resume: resumeSpy,
    get state() {
      return contextState;
    },
    decodeAudioData: decodeAudioDataSpy,
    close: vi.fn(),
    destination: {},
    currentTime: 0,
  };
});
vi.stubGlobal("AudioContext", audioContextSpy);

// A gate the tests can hold open, so a load can be caught mid-fetch — the
// window in which `disable()` can latch under a slow network or cache read.
let fetchGate: Promise<void> | null = null;
const fetchSpy = vi.fn().mockImplementation(async () => {
  if (fetchGate) await fetchGate;
  return { arrayBuffer: () => Promise.resolve(new ArrayBuffer(8)) };
});
vi.stubGlobal("fetch", fetchSpy);

// Avoid IndexedDB in happy-dom.
vi.mock("../audioCache", () => ({
  fetchWithCache: vi.fn().mockResolvedValue(new ArrayBuffer(8)),
  getCachedManifest: vi.fn().mockResolvedValue(null),
  cacheThemeManifest: vi.fn().mockResolvedValue(undefined),
  clearThemeCache: vi.fn().mockResolvedValue(undefined),
}));

async function freshAudioManager() {
  vi.resetModules();
  const { audioManager } = await import("../AudioManager");
  return audioManager;
}

describe("AudioManager.disable", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    contextState = "running";
    fetchGate = null;
    rejectPlay = null;
    playSpy.mockImplementation(
      () =>
        new Promise((_resolve, reject) => {
          rejectPlay = reject;
        }),
    );
  });

  // Disabled tests arm device-open first so they discriminate the disable()
  // gate specifically — unarmed, warmUp would no-op anyway.
  it("warmUp is a device-free no-op while disabled", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.disable();
    audioManager.warmUp();
    expect(audioContextSpy).not.toHaveBeenCalled();
    expect(audioManager.isDisabled).toBe(true);
  });

  // Control arm: proves the spy would fire if the gates were broken, so the
  // not-called assertions above and below are non-vacuous.
  it("armed and not disabled, the same fresh module DOES open the device", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    expect(audioContextSpy).toHaveBeenCalledOnce();
  });

  // Pre-verdict callers (e.g. Tab+Enter reaching VolumeControl through the
  // splash overlay) must not open the device before ensurePreload() arms it.
  it("before armDeviceOpen, warmUp/restart/ensurePlayback are device-free", async () => {
    const audioManager = await freshAudioManager();
    audioManager.warmUp();
    await audioManager.restart();
    audioManager.ensurePlayback();
    expect(audioContextSpy).not.toHaveBeenCalled();
    expect(audioManager.isDisabled).toBe(false);
  });

  it("restart stays device-free while disabled (dispose + warmUp cycle)", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.disable();
    await audioManager.restart();
    expect(audioContextSpy).not.toHaveBeenCalled();
  });

  it("ensurePlayback stays device-free while disabled", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.disable();
    audioManager.ensurePlayback();
    expect(audioContextSpy).not.toHaveBeenCalled();
  });

  it("preloadSfx skips without a context while disabled", async () => {
    const audioManager = await freshAudioManager();
    audioManager.disable();
    await expect(audioManager.preloadSfx()).resolves.toBe("skipped");
  });

  // "skipped" must stay distinct from "none": boot maps only "none" to a media
  // failure, so collapsing them would relabel a wedged-device boot.
  it("a preload with nothing to do is skipped, not a decode failure", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    await expect(audioManager.preloadSfx()).resolves.toBe("loaded");
    audioManager.disable();
    await expect(audioManager.preloadSfx()).resolves.toBe("skipped");
  });

  // Issue #6744's timeout latches `disabled` after warm-up, and buffers decoded
  // before the latch stay in the map — so the ctx/gain guards playSfx already
  // had cannot stop a later game event from starting a source on a stack we
  // just declared dead.
  it("playSfx starts no source once disabled, even with a buffer already loaded", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    await audioManager.preloadSfx();
    audioManager.disable();

    audioManager.playSfx("GameStarted");

    expect(createBufferSourceSpy).not.toHaveBeenCalled();
  });

  // Control arm: identical fixture, latch never set, so the assertion above is
  // about `disabled` and not about an empty buffer map.
  it("playSfx does start a source on the same loaded buffer while enabled", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    await audioManager.preloadSfx();

    audioManager.playSfx("GameStarted");

    expect(createBufferSourceSpy).toHaveBeenCalledOnce();
  });

  // Issue #6744 latches disable() AFTER warmUp() built a context, so the
  // null-ctx guard the test above rides on is gone. Without a `disabled` check
  // of its own, preloadSfx would keep opening decodes on a media stack that
  // never settles them.
  it("preloadSfx opens no decode once disabled, even with a live context", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    audioManager.disable();
    await audioManager.preloadSfx();
    expect(audioContextSpy).toHaveBeenCalledOnce();
    expect(decodeAudioDataSpy).not.toHaveBeenCalled();
  });

  // Control arm for the test above: same live context, latch never set, so the
  // not-called assertion is about `disabled` and not about the fixture.
  it("preloadSfx does decode on a live context while enabled", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    await audioManager.preloadSfx();
    expect(decodeAudioDataSpy).toHaveBeenCalled();
  });

  // The existing "ensurePlayback stays device-free while disabled" case above
  // disables BEFORE warm-up, so `ctx` is null and the resume branch is never
  // reached — it cannot see this. The boot deadline latches with the context
  // still live, and `ctx.resume()` acts on it directly rather than through a
  // guarded callee, so a gesture handler could walk back into the media path
  // boot just declared dead.
  it("ensurePlayback resumes nothing once disabled, even on a live suspended context", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    audioManager.disable();
    contextState = "suspended";

    audioManager.ensurePlayback();

    expect(audioContextSpy).toHaveBeenCalledOnce();
    expect(resumeSpy).not.toHaveBeenCalled();
  });

  // Control arm: identical live, suspended context with the latch never set.
  // Without it the assertion above would pass on a fixture that never reached
  // the resume branch at all.
  it("ensurePlayback does resume the same live suspended context while enabled", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    contextState = "suspended";

    audioManager.ensurePlayback();

    expect(resumeSpy).toHaveBeenCalledOnce();
  });

  // The `ensurePlayback` guard is synchronous and cannot see this: `playTrack`
  // initiates playback while enabled, and the rejection arrives later. If the
  // boot deadline latches in that window, the continuation would resume a media
  // stack that was deliberately declared dead.
  it("a play() rejection arriving after disable resumes nothing", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    audioManager.setContext("menu", true);

    // Reach guard: playback really was initiated, so there is a live
    // continuation for the latch to have to stop.
    expect(playSpy).toHaveBeenCalled();
    expect(rejectPlay).not.toBeNull();

    audioManager.disable();
    contextState = "suspended";
    rejectPlay!(new Error("autoplay blocked"));
    await flush();

    expect(resumeSpy).not.toHaveBeenCalled();
    // The retry rides behind resume(); neither may fire.
    expect(playSpy).toHaveBeenCalledOnce();
    warn.mockRestore();
  });

  // Control arm: identical deferred rejection on an identical suspended
  // context, latch never set. Without it the assertion above would pass on a
  // fixture whose continuation never ran at all.
  it("the same deferred rejection does resume while enabled", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    audioManager.setContext("menu", true);

    contextState = "suspended";
    rejectPlay!(new Error("autoplay blocked"));
    await flush();

    expect(resumeSpy).toHaveBeenCalledOnce();
    warn.mockRestore();
  });

  // Same window, different invalidation: a context change between initiating
  // playback and the rejection makes this continuation stale, so it must not
  // resume on behalf of a track that is no longer current.
  it("a play() rejection from a superseded generation resumes nothing", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    audioManager.setContext("menu", true);
    const staleReject = rejectPlay!;

    audioManager.setContext("battlefield", true);
    contextState = "suspended";
    staleReject(new Error("autoplay blocked"));
    await flush();

    expect(resumeSpy).not.toHaveBeenCalled();
    warn.mockRestore();
  });

  // `loadBuffer` checks the context before its fetch and then has to wait. If
  // the boot deadline latches while bytes are in flight, the continuation would
  // open the very decode pipeline boot declared unusable — the hang path again,
  // after a degraded start.
  it("a fetch landing after disable never opens a decode", async () => {
    let openGate!: () => void;
    fetchGate = new Promise<void>((resolve) => {
      openGate = resolve;
    });
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();

    const preload = audioManager.preloadSfx();
    await flush();
    // Reach guard: loads really are in flight and parked on the gate.
    expect(fetchSpy).toHaveBeenCalled();
    expect(decodeAudioDataSpy).not.toHaveBeenCalled();

    audioManager.disable();
    openGate();
    await preload;

    expect(decodeAudioDataSpy).not.toHaveBeenCalled();
  });

  // Control arm: the same held fetch released on the same schedule, latch never
  // set. Proves the continuation does reach decode, so the negative above is
  // about `disabled` and not about a gate that never opened.
  it("the same held fetch does reach decode while enabled", async () => {
    let openGate!: () => void;
    fetchGate = new Promise<void>((resolve) => {
      openGate = resolve;
    });
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();

    const preload = audioManager.preloadSfx();
    await flush();
    expect(decodeAudioDataSpy).not.toHaveBeenCalled();

    openGate();
    await preload;

    expect(decodeAudioDataSpy).toHaveBeenCalled();
  });

  // Same window, SFX-side invalidation: a theme switch clears the buffer map,
  // so a load started for the old theme must not repopulate it afterwards.
  it("a fetch landing after a theme switch does not repopulate the cleared map", async () => {
    let openGate!: () => void;
    fetchGate = new Promise<void>((resolve) => {
      openGate = resolve;
    });
    const audioManager = await freshAudioManager();
    const { PLANESWALKER_THEME } = await import("../planeswalkerTheme");
    audioManager.armDeviceOpen();
    audioManager.warmUp();

    const preload = audioManager.preloadSfx();
    await flush();
    expect(fetchSpy).toHaveBeenCalled();

    // Swap to a theme with no SFX at all; loadTheme clears the map.
    await audioManager.loadTheme({ ...PLANESWALKER_THEME, id: "other", sfx: [] });
    openGate();
    await preload;

    // `sfxAvailability()` cannot see this — it reports "skipped" for a theme
    // with no SFX whether or not stale buffers landed. Ask the buffer map the
    // way production does: a stale entry would give playSfx something to start.
    audioManager.playSfx("GameStarted");
    expect(createBufferSourceSpy).not.toHaveBeenCalled();
  });

  // Page navigation reaches setContext directly (useAudioContext on mount,
  // GameProvider on game transitions), so a route change after a degraded boot
  // must not start music. The guard lives at startMusic/playTrack rather than
  // at setContext — see the control arm below for why.
  it("setContext after disable starts no music source", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    audioManager.disable();

    audioManager.setContext("battlefield", true);

    expect(createMediaElementSourceSpy).not.toHaveBeenCalled();
    expect(playSpy).not.toHaveBeenCalled();
  });

  // Control arm: same live context and the same navigation call, latch unset.
  // Proves the negative above is about `disabled` and not about a fixture that
  // could never have started music.
  it("the same setContext does start music while enabled", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();

    audioManager.setContext("battlefield", true);

    expect(createMediaElementSourceSpy).toHaveBeenCalledOnce();
  });

  // Why setContext itself must NOT early-return on the latch: it is the
  // teardown path too. If the deadline latches while music is already playing,
  // a later navigation still has to stop it — an early return would leave a
  // "disabled" manager audible forever.
  it("setContext still stops music that was playing when the latch fired", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    audioManager.setContext("menu", true);
    // Reach guard: music really is playing before the latch.
    expect(audioManager.diagnostics()).toContain("music=playing");

    audioManager.disable();
    audioManager.setContext("battlefield", true);

    expect(audioManager.diagnostics()).toContain("music=stopped");
  });

  it("diagnostics reports the disabled state", async () => {
    const audioManager = await freshAudioManager();
    audioManager.disable();
    expect(audioManager.diagnostics()).toContain("ctx=disabled");
  });
});
