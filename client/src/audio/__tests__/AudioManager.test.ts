import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../stores/preferencesStore";

// --- Web Audio API mocks ---

const mockGainNode = () => ({
  gain: {
    value: 1,
    cancelScheduledValues: vi.fn(),
    setValueAtTime: vi.fn(),
    linearRampToValueAtTime: vi.fn(),
  },
  connect: vi.fn(),
});

const mockBufferSource = () => ({
  buffer: null as AudioBuffer | null,
  connect: vi.fn(),
  start: vi.fn(),
});

const mockMediaElementSource = () => ({
  connect: vi.fn(),
});

const mockAudioBuffer = {} as AudioBuffer;

const mockDecodeAudioData = vi.fn().mockResolvedValue(mockAudioBuffer);
const mockCreateGain = vi.fn().mockImplementation(mockGainNode);
const mockCreateBufferSource = vi.fn().mockImplementation(mockBufferSource);
const mockCreateMediaElementSource = vi
  .fn()
  .mockImplementation(mockMediaElementSource);
const mockClose = vi.fn();

vi.stubGlobal(
  "AudioContext",
  vi.fn().mockImplementation(() => ({
    createGain: mockCreateGain,
    createBufferSource: mockCreateBufferSource,
    createMediaElementSource: mockCreateMediaElementSource,
    decodeAudioData: mockDecodeAudioData,
    close: mockClose,
    destination: {},
    currentTime: 0,
  })),
);

// Mock fetch for SFX preloading
const mockArrayBuffer = new ArrayBuffer(8);
vi.stubGlobal(
  "fetch",
  vi.fn().mockResolvedValue({
    arrayBuffer: () => Promise.resolve(mockArrayBuffer),
  }),
);

// Mock HTMLAudioElement
const mockAudioPlay = vi.fn().mockResolvedValue(undefined);
const mockAudioPause = vi.fn();

vi.stubGlobal(
  "Audio",
  vi.fn().mockImplementation(() => ({
    play: mockAudioPlay,
    pause: mockAudioPause,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })),
);

// Import after mocks are set up
import { audioManager, initAudioOnInteraction } from "../AudioManager";

describe("AudioManager", () => {
  beforeEach(() => {
    vi.clearAllMocks();

    // Reset preferences to defaults
    act(() => {
      usePreferencesStore.setState({
        sfxVolume: 70,
        musicVolume: 40,
        sfxMuted: false,
        musicMuted: false,
        masterMuted: false,
      });
    });
  });

  afterEach(() => {
    audioManager.dispose();
  });

  // --- warmUp ---

  it("warmUp creates AudioContext and gain nodes", () => {
    audioManager.warmUp();

    expect(AudioContext).toHaveBeenCalledOnce();
    // Two gain nodes: sfxGain + musicGain
    expect(mockCreateGain).toHaveBeenCalledTimes(2);
  });

  it("warmUp applies saved volume preferences", () => {
    act(() => {
      usePreferencesStore.setState({ sfxVolume: 50, musicVolume: 30 });
    });

    audioManager.warmUp();

    const gains = mockCreateGain.mock.results;
    // First gain = sfxGain, second = musicGain
    expect(gains[0].value.gain.value).toBe(0.5);
    expect(gains[1].value.gain.value).toBe(0.3);
  });

  // --- preloadSfx ---

  it("preloadSfx fetches and decodes all unique SFX files", async () => {
    audioManager.warmUp();
    await audioManager.preloadSfx();

    // SFX_MAP has 15 entries but some share the same filename (e.g., "destroy" x3)
    // Unique filenames: destroy, life_loss, instant, creature, block, green_land,
    // draw, sorcery, token, shuffle, win_duel, add_counter, enchant = 13
    expect(fetch).toHaveBeenCalledTimes(13);
    expect(mockDecodeAudioData).toHaveBeenCalledTimes(13);
  });

  it("preloadSfx handles individual fetch failures gracefully", async () => {
    const consoleSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.mocked(fetch)
      .mockResolvedValueOnce({
        arrayBuffer: () => Promise.resolve(mockArrayBuffer),
      } as Response)
      .mockRejectedValueOnce(new Error("network error"))
      .mockResolvedValue({
        arrayBuffer: () => Promise.resolve(mockArrayBuffer),
      } as Response);

    audioManager.warmUp();
    await audioManager.preloadSfx();

    expect(consoleSpy).toHaveBeenCalled();
    // Should not throw -- remaining files still loaded
    consoleSpy.mockRestore();
  });

  // --- playSfx ---

  it("playSfx creates BufferSourceNode and starts it for mapped events", async () => {
    audioManager.warmUp();
    await audioManager.preloadSfx();

    audioManager.playSfx("DamageDealt");

    expect(mockCreateBufferSource).toHaveBeenCalled();
    const source = mockCreateBufferSource.mock.results[0].value;
    expect(source.start).toHaveBeenCalled();
  });

  it("playSfx does nothing for unmapped event types", async () => {
    audioManager.warmUp();
    await audioManager.preloadSfx();
    mockCreateBufferSource.mockClear();

    audioManager.playSfx("PhaseChanged");

    expect(mockCreateBufferSource).not.toHaveBeenCalled();
  });

  it("playSfx does nothing when sfxMuted", async () => {
    audioManager.warmUp();
    await audioManager.preloadSfx();
    mockCreateBufferSource.mockClear();

    act(() => {
      usePreferencesStore.setState({ sfxMuted: true });
    });

    audioManager.playSfx("DamageDealt");

    expect(mockCreateBufferSource).not.toHaveBeenCalled();
  });

  it("playSfx does nothing when masterMuted", async () => {
    audioManager.warmUp();
    await audioManager.preloadSfx();
    mockCreateBufferSource.mockClear();

    act(() => {
      usePreferencesStore.setState({ masterMuted: true });
    });

    audioManager.playSfx("DamageDealt");

    expect(mockCreateBufferSource).not.toHaveBeenCalled();
  });

  // --- playSfxForStep ---

  it("playSfxForStep consolidates same-type effects into single sound with boosted volume", async () => {
    audioManager.warmUp();
    await audioManager.preloadSfx();
    mockCreateBufferSource.mockClear();
    mockCreateGain.mockClear();

    audioManager.playSfxForStep([
      { event: { type: "CreatureDestroyed", data: { object_id: 1 } }, duration: 400 },
      { event: { type: "CreatureDestroyed", data: { object_id: 2 } }, duration: 400 },
      { event: { type: "CreatureDestroyed", data: { object_id: 3 } }, duration: 400 },
    ]);

    // Single consolidated sound, not 3 separate ones
    expect(mockCreateBufferSource).toHaveBeenCalledTimes(1);
    // Volume boost gain node created (count=3 -> 1.0 + 3*0.15 = 1.45)
    expect(mockCreateGain).toHaveBeenCalled();
    const boostGain = mockCreateGain.mock.results[0].value;
    expect(boostGain.gain.value).toBe(1.45);
  });

  it("playSfxForStep plays distinct sounds for different effect types in same step", async () => {
    audioManager.warmUp();
    await audioManager.preloadSfx();
    mockCreateBufferSource.mockClear();

    audioManager.playSfxForStep([
      { event: { type: "DamageDealt", data: { source_id: 1, target: { Player: 0 }, amount: 3 } }, duration: 600 },
      { event: { type: "LifeChanged", data: { player_id: 0, amount: -3 } }, duration: 600 },
    ]);

    // Two different SFX types -> two sounds
    expect(mockCreateBufferSource).toHaveBeenCalledTimes(2);
  });

  // --- startMusic ---

  it("startMusic creates HTMLAudioElement and plays", () => {
    audioManager.warmUp();
    audioManager.startMusic();

    expect(Audio).toHaveBeenCalled();
    expect(mockAudioPlay).toHaveBeenCalled();
    expect(mockCreateMediaElementSource).toHaveBeenCalled();
  });

  // --- stopMusic ---

  it("stopMusic fades out and pauses", () => {
    vi.useFakeTimers();

    audioManager.warmUp();
    audioManager.startMusic();
    audioManager.stopMusic(1.0);

    // Gain ramp scheduled
    const musicGain = mockCreateGain.mock.results[1].value;
    expect(musicGain.gain.linearRampToValueAtTime).toHaveBeenCalledWith(
      0,
      expect.any(Number),
    );

    // After fade duration, audio is paused
    vi.advanceTimersByTime(1000);
    expect(mockAudioPause).toHaveBeenCalled();

    vi.useRealTimers();
  });

  // --- updateVolumes ---

  it("updateVolumes sets gain values from preferences", () => {
    audioManager.warmUp();

    act(() => {
      usePreferencesStore.setState({ sfxVolume: 50, musicVolume: 80 });
    });

    audioManager.updateVolumes();

    const sfxGain = mockCreateGain.mock.results[0].value;
    const musicGain = mockCreateGain.mock.results[1].value;

    expect(sfxGain.gain.value).toBe(0.5);
    expect(musicGain.gain.value).toBe(0.8);
  });

  it("updateVolumes sets gains to 0 when masterMuted", () => {
    audioManager.warmUp();

    act(() => {
      usePreferencesStore.setState({ masterMuted: true });
    });

    audioManager.updateVolumes();

    const sfxGain = mockCreateGain.mock.results[0].value;
    const musicGain = mockCreateGain.mock.results[1].value;

    expect(sfxGain.gain.value).toBe(0);
    expect(musicGain.gain.value).toBe(0);
  });

  // --- initAudioOnInteraction ---

  it("initAudioOnInteraction attaches event listeners and removes them after first fire", () => {
    const addSpy = vi.spyOn(document, "addEventListener");
    const removeSpy = vi.spyOn(document, "removeEventListener");

    initAudioOnInteraction();

    expect(addSpy).toHaveBeenCalledWith("click", expect.any(Function));
    expect(addSpy).toHaveBeenCalledWith("touchstart", expect.any(Function));
    expect(addSpy).toHaveBeenCalledWith("keydown", expect.any(Function));

    // Simulate a click
    const clickHandler = addSpy.mock.calls.find((c) => c[0] === "click")![1];
    (clickHandler as EventListener)(new Event("click"));

    // All listeners should be removed
    expect(removeSpy).toHaveBeenCalledWith("click", expect.any(Function));
    expect(removeSpy).toHaveBeenCalledWith("touchstart", expect.any(Function));
    expect(removeSpy).toHaveBeenCalledWith("keydown", expect.any(Function));

    // AudioContext should have been created
    expect(AudioContext).toHaveBeenCalled();

    addSpy.mockRestore();
    removeSpy.mockRestore();
  });
});
