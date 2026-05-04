import { useCallback, useEffect, useRef, useState } from "react";

import { audioManager } from "../../audio/AudioManager.ts";
import { cacheThemeManifest, clearThemeCache } from "../../audio/audioCache.ts";
import { BUILT_IN_THEMES, findManifest, validateThemeManifest } from "../../audio/themeRegistry.ts";
import { PLANESWALKER_THEME } from "../../audio/planeswalkerTheme.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import {
  ANIMATION_SPEED_DEFAULT,
  ANIMATION_SPEED_MAX,
  ANIMATION_SPEED_MIN,
  ANIMATION_SPEED_STEP,
  PACING_CATEGORIES,
  PACING_DEFAULT,
  PACING_DESCRIPTIONS,
  PACING_LABELS,
  PACING_MAX,
  PACING_MIN,
  PACING_STEP,
  type PacingCategory,
  type VfxQuality,
} from "../../animation/types.ts";
import type {
  CardSizePreference,
  LogDefaultState,
} from "../../stores/preferencesStore.ts";
import { BATTLEFIELDS } from "../board/battlefields.ts";
import { PLAIN_BACKGROUNDS } from "../board/plainBackgrounds.ts";
import { ModalPanelShell } from "../ui/ModalPanelShell";
import { downloadBackup, importBackupFromFile, type ImportMode } from "../../services/backup.ts";

export type SettingsHighlight = "board-background";

interface PreferencesModalProps {
  onClose: () => void;
  initialTab?: SettingsTabId;
  highlight?: SettingsHighlight;
}

const CARD_SIZES: CardSizePreference[] = ["small", "medium", "large"];
const LOG_DEFAULTS: LogDefaultState[] = ["open", "closed"];
const VFX_QUALITIES: VfxQuality[] = ["full", "reduced", "minimal"];

/** Format a speed value as a user-facing label. The slider goes 0→max where
 *  max = instant (skip animations). `0` = slowest, `1` = normal. */
function formatSpeed(value: number, max: number): string {
  if (value >= max) return "Instant";
  if (value <= 0) return "Slowest";
  return `${value.toFixed(2)}×`;
}
const SETTINGS_TABS = [
  { id: "gameplay", label: "Gameplay" },
  { id: "visual", label: "Visual" },
  { id: "combat", label: "Pacing" },
  { id: "audio", label: "Audio" },
  { id: "multiplayer", label: "Multiplayer" },
  { id: "data", label: "Data" },
  { id: "experimental", label: "Experimental" },
] as const;

export type SettingsTabId = (typeof SETTINGS_TABS)[number]["id"];

const BOARD_BACKGROUND_GROUPS: { label: string; options: { value: string; label: string }[] }[] = [
  {
    label: "Automatic",
    options: [
      { value: "auto-wubrg", label: "Auto (match deck)" },
      { value: "random", label: "Random" },
    ],
  },
  {
    label: "Battlefields",
    options: BATTLEFIELDS.map((bf) => ({ value: bf.id, label: `${bf.label} (${bf.color})` })),
  },
  {
    label: "Plain",
    options: PLAIN_BACKGROUNDS.map((bg) => ({ value: bg.id, label: bg.label })),
  },
  {
    label: "Custom",
    options: [{ value: "custom", label: "Custom URL" }],
  },
  {
    label: "Off",
    options: [{ value: "none", label: "None" }],
  },
];

export function PreferencesModal({
  onClose,
  initialTab = "gameplay",
  highlight,
}: PreferencesModalProps) {
  const boardBackgroundRef = useRef<HTMLDivElement | null>(null);
  const [highlightFlash, setHighlightFlash] = useState(highlight === "board-background");

  useEffect(() => {
    if (highlight !== "board-background") return;
    // Scroll the highlighted section into view and flash a ring outline briefly.
    const frame = requestAnimationFrame(() => {
      boardBackgroundRef.current?.scrollIntoView({ behavior: "smooth", block: "center" });
    });
    const timer = window.setTimeout(() => setHighlightFlash(false), 1800);
    return () => {
      cancelAnimationFrame(frame);
      window.clearTimeout(timer);
    };
  }, [highlight]);

  const cardSize = usePreferencesStore((s) => s.cardSize);
  const logDefaultState = usePreferencesStore((s) => s.logDefaultState);
  const boardBackground = usePreferencesStore((s) => s.boardBackground);
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const animationSpeedMultiplier = usePreferencesStore((s) => s.animationSpeedMultiplier);
  const pacingMultipliers = usePreferencesStore((s) => s.pacingMultipliers);
  const setCardSize = usePreferencesStore((s) => s.setCardSize);
  const setLogDefaultState = usePreferencesStore((s) => s.setLogDefaultState);
  const setBoardBackground = usePreferencesStore((s) => s.setBoardBackground);
  const customBackgroundUrl = usePreferencesStore((s) => s.customBackgroundUrl);
  const setCustomBackgroundUrl = usePreferencesStore((s) => s.setCustomBackgroundUrl);
  const setVfxQuality = usePreferencesStore((s) => s.setVfxQuality);
  const setPacingMultiplier = usePreferencesStore((s) => s.setPacingMultiplier);
  const resetPacing = usePreferencesStore((s) => s.resetPacing);
  const resetAllPreferences = usePreferencesStore((s) => s.resetAllPreferences);
  const masterVolume = usePreferencesStore((s) => s.masterVolume);
  const sfxVolume = usePreferencesStore((s) => s.sfxVolume);
  const musicVolume = usePreferencesStore((s) => s.musicVolume);
  const masterMuted = usePreferencesStore((s) => s.masterMuted);
  const setMasterMuted = usePreferencesStore((s) => s.setMasterMuted);
  const setMasterVolume = usePreferencesStore((s) => s.setMasterVolume);
  const setSfxVolume = usePreferencesStore((s) => s.setSfxVolume);
  const setMusicVolume = usePreferencesStore((s) => s.setMusicVolume);
  const setAnimationSpeedMultiplier = usePreferencesStore((s) => s.setAnimationSpeedMultiplier);
  const showKeywordStrip = usePreferencesStore((s) => s.showKeywordStrip) ?? true;
  const setShowKeywordStrip = usePreferencesStore((s) => s.setShowKeywordStrip);

  // Audio theme settings
  const audioThemeId = usePreferencesStore((s) => s.audioThemeId);
  const customThemeUrls = usePreferencesStore((s) => s.customThemeUrls);
  const setAudioThemeId = usePreferencesStore((s) => s.setAudioThemeId);
  const addCustomThemeUrl = usePreferencesStore((s) => s.addCustomThemeUrl);
  const removeCustomThemeUrl = usePreferencesStore((s) => s.removeCustomThemeUrl);
  const [themeImportUrl, setThemeImportUrl] = useState("");
  const [themeImportStatus, setThemeImportStatus] = useState<"idle" | "loading" | "error">("idle");
  const [themeImportError, setThemeImportError] = useState("");

  const handleThemeChange = useCallback(async (id: string) => {
    setAudioThemeId(id);
    try {
      const manifest = await findManifest(id, customThemeUrls);
      await audioManager.loadTheme(manifest);
    } catch {
      // Fallback to planeswalker on failure
      setAudioThemeId("planeswalker");
      await audioManager.loadTheme(PLANESWALKER_THEME);
    }
  }, [setAudioThemeId, customThemeUrls]);

  const handleImportTheme = useCallback(async () => {
    if (!themeImportUrl.trim()) return;
    setThemeImportStatus("loading");
    setThemeImportError("");
    try {
      const response = await fetch(themeImportUrl.trim());
      const json: unknown = await response.json();
      const result = validateThemeManifest(json);
      if (result instanceof Error) throw result;
      addCustomThemeUrl(result.id, themeImportUrl.trim());
      await cacheThemeManifest(result.id, result);
      setThemeImportUrl("");
      setThemeImportStatus("idle");
    } catch (err) {
      setThemeImportError(err instanceof Error ? err.message : "Failed to import theme");
      setThemeImportStatus("error");
    }
  }, [themeImportUrl, addCustomThemeUrl]);

  const handleRemoveTheme = useCallback(async (id: string) => {
    removeCustomThemeUrl(id);
    await clearThemeCache(id);
    if (audioThemeId === id) {
      await audioManager.loadTheme(PLANESWALKER_THEME);
    }
  }, [removeCustomThemeUrl, audioThemeId]);

  // Multiplayer settings — server picking lives in `ServerPicker` (opened
  // from the lobby header in either server or P2P mode), not here.
  const displayName = useMultiplayerStore((s) => s.displayName);
  const setDisplayName = useMultiplayerStore((s) => s.setDisplayName);
  const [activeTab, setActiveTab] = useState<SettingsTabId>(initialTab);

  return (
    <ModalPanelShell
      title="Settings"
      subtitle="Tune gameplay, visuals, audio, and multiplayer defaults."
      onClose={onClose}
      maxWidthClassName="max-w-5xl"
      bodyClassName="overflow-y-auto p-4 sm:p-6"
    >
      <div className="grid gap-4 md:grid-cols-[200px_minmax(0,1fr)]">
            <nav className="flex snap-x gap-2 overflow-x-auto pb-1 md:flex-col md:overflow-visible md:pb-0">
              {SETTINGS_TABS.map((tab) => (
                <button
                  key={tab.id}
                  onClick={() => setActiveTab(tab.id)}
                  className={`min-h-11 shrink-0 snap-start rounded-[16px] border px-3 py-2.5 text-left text-[11px] font-semibold uppercase tracking-[0.16em] transition-colors md:w-full md:px-4 md:text-xs md:tracking-[0.18em] ${
                    activeTab === tab.id
                      ? "border-sky-400/60 bg-sky-500/14 text-sky-100"
                      : "border-white/8 bg-black/20 text-slate-400 hover:border-white/14 hover:text-slate-100"
                  }`}
                >
                  {tab.label}
                </button>
              ))}
            </nav>

            <div className="min-w-0">
              {activeTab === "gameplay" && (
                <SettingsSection title="Gameplay">
                  <SettingGroup label="Card Size">
                    <SegmentedControl
                      options={CARD_SIZES}
                      value={cardSize}
                      onChange={setCardSize}
                    />
                  </SettingGroup>

                  <SettingGroup label="Log Default">
                    <SegmentedControl
                      options={LOG_DEFAULTS}
                      value={logDefaultState}
                      onChange={setLogDefaultState}
                    />
                  </SettingGroup>

                  <div
                    ref={boardBackgroundRef}
                    className={`-m-1 rounded-[16px] p-1 transition-shadow duration-500 ${
                      highlightFlash
                        ? "shadow-[0_0_0_2px_rgba(56,189,248,0.8),0_0_24px_rgba(56,189,248,0.35)]"
                        : ""
                    }`}
                  >
                    <SettingGroup label="Board Background">
                      <select
                        value={boardBackground}
                        onChange={(e) => setBoardBackground(e.target.value)}
                        className="w-full rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 focus:border-sky-400/40 focus:outline-none"
                      >
                        {BOARD_BACKGROUND_GROUPS.map((group) => (
                          <optgroup key={group.label} label={group.label}>
                            {group.options.map((bg) => (
                              <option key={bg.value} value={bg.value}>
                                {bg.label}
                              </option>
                            ))}
                          </optgroup>
                        ))}
                      </select>
                      {boardBackground === "custom" && (
                        <input
                          type="url"
                          value={customBackgroundUrl}
                          onChange={(e) => setCustomBackgroundUrl(e.target.value)}
                          placeholder="https://example.com/image.jpg"
                          className="mt-2 w-full rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 placeholder:text-slate-500 focus:border-sky-400/40 focus:outline-none"
                        />
                      )}
                    </SettingGroup>
                  </div>
                </SettingsSection>
              )}

              {activeTab === "visual" && (
                <SettingsSection title="Visual">
                  <SettingGroup label="VFX Quality">
                    <SegmentedControl
                      options={VFX_QUALITIES}
                      value={vfxQuality}
                      onChange={setVfxQuality}
                    />
                  </SettingGroup>

                  <SettingGroup label="Keyword Strip">
                    <label className="flex min-h-11 items-center gap-2">
                      <input
                        type="checkbox"
                        checked={showKeywordStrip}
                        onChange={(e) => setShowKeywordStrip(e.target.checked)}
                        className="accent-cyan-500"
                      />
                      <span className="text-sm text-slate-200">Show keywords on battlefield cards</span>
                    </label>
                  </SettingGroup>
                </SettingsSection>
              )}

              {activeTab === "combat" && (
                <PacingSection
                  animationSpeedMultiplier={animationSpeedMultiplier}
                  setAnimationSpeedMultiplier={setAnimationSpeedMultiplier}
                  pacingMultipliers={pacingMultipliers}
                  setPacingMultiplier={setPacingMultiplier}
                  resetPacing={resetPacing}
                />
              )}

              {activeTab === "audio" && (<>
                <SettingsSection title="Audio">
                  <SettingGroup label="Mute All">
                    <label className="flex min-h-11 items-center gap-2">
                      <input
                        type="checkbox"
                        checked={masterMuted}
                        onChange={(e) => {
                          setMasterMuted(e.target.checked);
                          if (!e.target.checked) audioManager.ensurePlayback();
                        }}
                        className="accent-cyan-500"
                      />
                      <span className="text-sm text-slate-200">Mute all audio</span>
                    </label>
                  </SettingGroup>

                  <SettingGroup label="Global Volume">
                    <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
                      <input
                        type="range"
                        min={0}
                        max={100}
                        value={masterVolume}
                        onChange={(e) => setMasterVolume(Number(e.target.value))}
                        className="flex-1 accent-cyan-500"
                      />
                      <span className="text-xs text-slate-400 sm:w-10 sm:text-right">{masterVolume}%</span>
                    </div>
                  </SettingGroup>

                  <SettingGroup label="SFX Volume">
                    <div className={`flex flex-col gap-2 sm:flex-row sm:items-center ${masterMuted ? "opacity-50" : ""}`}>
                      <input
                        type="range"
                        min={0}
                        max={100}
                        value={sfxVolume}
                        onChange={(e) => setSfxVolume(Number(e.target.value))}
                        className="flex-1 accent-cyan-500"
                      />
                      <span className="text-xs text-slate-400 sm:w-10 sm:text-right">{sfxVolume}%</span>
                    </div>
                  </SettingGroup>

                  <SettingGroup label="Music Volume">
                    <div className={`flex flex-col gap-2 sm:flex-row sm:items-center ${masterMuted ? "opacity-50" : ""}`}>
                      <input
                        type="range"
                        min={0}
                        max={100}
                        value={musicVolume}
                        onChange={(e) => setMusicVolume(Number(e.target.value))}
                        className="flex-1 accent-cyan-500"
                      />
                      <span className="text-xs text-slate-400 sm:w-10 sm:text-right">{musicVolume}%</span>
                    </div>
                  </SettingGroup>
                </SettingsSection>

                <SettingsSection title="Audio Theme">
                  <SettingGroup label="Theme">
                    <select
                      value={audioThemeId}
                      onChange={(e) => handleThemeChange(e.target.value)}
                      className="w-full rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 focus:border-sky-400/40 focus:outline-none"
                    >
                      {Object.values(BUILT_IN_THEMES).map((t) => (
                        <option key={t.id} value={t.id}>{t.name}</option>
                      ))}
                      {customThemeUrls.map((t) => (
                        <option key={t.id} value={t.id}>{t.id}</option>
                      ))}
                    </select>
                  </SettingGroup>

                  <SettingGroup label="Import Theme">
                    <div className="flex flex-col gap-2">
                      <div className="flex gap-2">
                        <input
                          type="text"
                          value={themeImportUrl}
                          onChange={(e) => setThemeImportUrl(e.target.value)}
                          placeholder="https://example.com/theme.json"
                          className="min-h-11 flex-1 rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 placeholder-slate-500 focus:border-sky-400/40 focus:outline-none"
                        />
                        <button
                          type="button"
                          onClick={handleImportTheme}
                          disabled={themeImportStatus === "loading" || !themeImportUrl.trim()}
                          className="rounded-[14px] border border-white/10 bg-sky-600/30 px-4 py-2 text-sm text-slate-100 hover:bg-sky-600/50 disabled:opacity-50"
                        >
                          {themeImportStatus === "loading" ? "Loading..." : "Import"}
                        </button>
                      </div>
                      {themeImportStatus === "error" && (
                        <p className="text-xs text-red-400">{themeImportError}</p>
                      )}
                    </div>
                  </SettingGroup>

                  {customThemeUrls.length > 0 && (
                    <SettingGroup label="Custom Themes">
                      <div className="flex flex-col gap-1">
                        {customThemeUrls.map((t) => (
                          <div key={t.id} className="flex items-center justify-between rounded-lg bg-black/20 px-3 py-2">
                            <span className="text-sm text-slate-300">{t.id}</span>
                            <button
                              type="button"
                              onClick={() => handleRemoveTheme(t.id)}
                              className="text-xs text-red-400 hover:text-red-300"
                            >
                              Remove
                            </button>
                          </div>
                        ))}
                      </div>
                    </SettingGroup>
                  )}
                </SettingsSection>
              </>)}

              {activeTab === "multiplayer" && (
                <SettingsSection title="Multiplayer">
                  <SettingGroup label="Display Name">
                      <input
                        type="text"
                        value={displayName}
                        onChange={(e) => setDisplayName(e.target.value)}
                        placeholder="Enter your name"
                        maxLength={20}
                        className="w-full rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 placeholder-slate-500 focus:border-sky-400/40 focus:outline-none"
                      />
                  </SettingGroup>

                  <p className="text-xs text-slate-400">
                    Server selection moved to the lobby — open Multiplayer and use
                    the server chip (or "Pick server" in P2P mode) to switch
                    regions, configure a self-hosted instance, or test connectivity.
                  </p>
                </SettingsSection>
              )}

              {activeTab === "data" && <DataSection />}

              {activeTab === "experimental" && <ExperimentalSection />}
            </div>
            <ResetAllFooter resetAllPreferences={resetAllPreferences} />
          </div>
    </ModalPanelShell>
  );
}

/** Discreet trailing footer with a single "Reset to defaults" action that
 *  wipes the entire preferences store back to factory defaults. Confirms
 *  before firing — this clears AI seats, board background, audio levels,
 *  and every pacing slider, which is rarely what someone means accidentally. */
function ResetAllFooter({
  resetAllPreferences,
}: {
  resetAllPreferences: () => void;
}) {
  const onClick = useCallback(() => {
    if (window.confirm("Reset all preferences to defaults? This clears every setting in this dialog.")) {
      resetAllPreferences();
    }
  }, [resetAllPreferences]);

  return (
    <div className="mt-4 flex justify-end border-t border-white/5 pt-3">
      <button
        type="button"
        onClick={onClick}
        className="text-xs font-medium uppercase tracking-[0.18em] text-slate-500 transition-colors hover:text-rose-300"
      >
        Reset all preferences
      </button>
    </div>
  );
}

function ExperimentalSection() {
  const experimentalFeatures = usePreferencesStore((s) => s.experimentalFeatures);
  const setExperimentalFeatures = usePreferencesStore((s) => s.setExperimentalFeatures);
  const useGridLayout = usePreferencesStore((s) => s.useGridLayout);
  const setUseGridLayout = usePreferencesStore((s) => s.setUseGridLayout);

  return (
    <SettingsSection title="Experimental">
      <p className="text-xs text-slate-400">
        These features are still in development. They may be incomplete, buggy,
        or change without notice. Enable them to try things out early.
      </p>

      <SettingGroup label="Draft Mode">
        <label className="flex min-h-11 items-center gap-3">
          <input
            type="checkbox"
            checked={experimentalFeatures}
            onChange={(e) => setExperimentalFeatures(e.target.checked)}
            className="accent-cyan-500"
          />
          <div className="flex flex-col">
            <span className="text-sm text-slate-200">Enable draft</span>
            <span className="text-xs text-slate-500">
              Unlocks Quick Draft and Pod Draft on the main menu.
            </span>
          </div>
        </label>
      </SettingGroup>

      <SettingGroup label="Board Layout">
        <label className="flex min-h-11 items-center gap-3">
          <input
            type="checkbox"
            checked={useGridLayout}
            onChange={(e) => setUseGridLayout(e.target.checked)}
            className="accent-cyan-500"
          />
          <div className="flex flex-col">
            <span className="text-sm text-slate-200">CSS Grid layout</span>
            <span className="text-xs text-slate-500">
              Uses CSS Grid for the game board instead of flexbox. Improved mobile spacing and HUD positioning.
            </span>
          </div>
        </label>
      </SettingGroup>
    </SettingsSection>
  );
}

function DataSection() {
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const onExport = useCallback(() => {
    setError(null);
    try {
      downloadBackup();
      setStatus("Backup downloaded.");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const onImport = useCallback(
    async (file: File, mode: ImportMode) => {
      setError(null);
      setStatus(null);
      try {
        const result = await importBackupFromFile(file, mode);
        const base =
          `Imported ${result.decksImported} deck(s)` +
          (result.preferencesReplaced ? " and preferences." : ".");
        const malformedSuffix =
          result.malformedKeys.length > 0
            ? ` Skipped ${result.malformedKeys.length} malformed entr${result.malformedKeys.length === 1 ? "y" : "ies"}.`
            : "";
        setStatus(base + malformedSuffix);
        // Zustand stores read from localStorage at boot — reload so every
        // subscriber picks up the restored data instead of holding stale state.
        setTimeout(() => {
          window.location.reload();
        }, 600);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [],
  );

  return (
    <SettingsSection title="Backup & Restore">
      <p className="text-xs text-slate-400">
        Export bundles your preferences, imported decks, and feed subscriptions
        into a single JSON file. Import restores them on another machine. IndexedDB
        caches (feed cache, audio cache, saved games) are not included — those
        rebuild automatically.
      </p>
      <div className="flex flex-wrap gap-2">
        <button
          onClick={onExport}
          className="rounded-[14px] border border-white/10 bg-white/5 px-4 py-2 text-sm font-medium text-slate-100 transition hover:bg-white/10"
        >
          Export backup…
        </button>
        <button
          onClick={() => {
            fileInputRef.current?.click();
          }}
          className="rounded-[14px] border border-white/10 bg-white/5 px-4 py-2 text-sm font-medium text-slate-100 transition hover:bg-white/10"
        >
          Import backup…
        </button>
      </div>
      <input
        ref={fileInputRef}
        type="file"
        accept="application/json,.json"
        className="hidden"
        onChange={(e) => {
          const file = e.target.files?.[0];
          e.target.value = "";
          if (!file) return;
          const mode: ImportMode = window.confirm(
            "Overwrite existing preferences and decks?\n\n" +
              "OK: replace everything with the backup (destructive).\n" +
              "Cancel: merge — keep existing decks, add new ones from the backup.",
          )
            ? "overwrite"
            : "merge";
          void onImport(file, mode);
        }}
      />
      {status && <p className="text-xs text-emerald-400">{status}</p>}
      {error && <p className="text-xs text-rose-400">{error}</p>}
    </SettingsSection>
  );
}

function SettingsSection({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-[20px] border border-white/10 bg-black/18 p-4 shadow-[0_18px_54px_rgba(0,0,0,0.18)] backdrop-blur-md sm:p-5">
      <h3 className="mb-4 text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-slate-500">{title}</h3>
      <div className="flex flex-col gap-4">{children}</div>
    </section>
  );
}

function SettingGroup({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="mb-2 block text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
        {label}
      </label>
      {children}
    </div>
  );
}

/** Single labelled multiplier slider with an inline reset affordance. The
 *  reset button is rendered always (so the layout doesn't shift), but is
 *  visually faded and disabled when the value already equals `defaultValue`.
 *  Aria + tooltip wired so screen readers and hover-help both work. */
function MultiplierSlider({
  label,
  description,
  value,
  defaultValue,
  min,
  max,
  step,
  onChange,
}: {
  label: string;
  description?: string;
  value: number;
  defaultValue: number;
  min: number;
  max: number;
  step: number;
  onChange: (next: number) => void;
}) {
  const atDefault = Math.abs(value - defaultValue) < 1e-9;
  return (
    <div>
      <div className="mb-2 flex items-baseline justify-between gap-3">
        <label className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
          {label}
        </label>
        <span className="font-mono text-xs tabular-nums text-slate-300">
          {formatSpeed(value, max)}
        </span>
      </div>
      <div className="flex items-center gap-2">
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(e) => onChange(Number(e.target.value))}
          onDoubleClick={() => onChange(defaultValue)}
          aria-label={label}
          className="flex-1 accent-cyan-500"
        />
        <button
          type="button"
          onClick={() => onChange(defaultValue)}
          disabled={atDefault}
          aria-label={`Reset ${label} to default`}
          title={atDefault ? "At default" : "Reset to default"}
          className={`flex h-7 w-7 items-center justify-center rounded-full border border-white/10 bg-black/18 text-slate-300 transition-all ${
            atDefault
              ? "cursor-not-allowed opacity-30"
              : "hover:border-cyan-400/40 hover:text-cyan-200 hover:bg-cyan-400/10"
          }`}
        >
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={2}
            strokeLinecap="round"
            strokeLinejoin="round"
            className="h-3.5 w-3.5"
            aria-hidden="true"
          >
            <path d="M3 12a9 9 0 1 0 3-6.7" />
            <path d="M3 4v5h5" />
          </svg>
        </button>
      </div>
      {description && <p className="mt-2 text-xs text-slate-500">{description}</p>}
    </div>
  );
}

/** Unified pacing panel — global animation speed plus every per-category
 *  multiplier in one place. Each slider has its own reset; the section also
 *  offers a "Reset section" link that resets everything here without
 *  touching unrelated preferences. */
function PacingSection({
  animationSpeedMultiplier,
  setAnimationSpeedMultiplier,
  pacingMultipliers,
  setPacingMultiplier,
  resetPacing,
}: {
  animationSpeedMultiplier: number;
  setAnimationSpeedMultiplier: (n: number) => void;
  pacingMultipliers: Record<PacingCategory, number>;
  setPacingMultiplier: (category: PacingCategory, n: number) => void;
  resetPacing: () => void;
}) {
  const allAtDefault =
    Math.abs(animationSpeedMultiplier - ANIMATION_SPEED_DEFAULT) < 1e-9 &&
    PACING_CATEGORIES.every(
      (c) => Math.abs(pacingMultipliers[c] - PACING_DEFAULT) < 1e-9,
    );

  return (
    <section className="rounded-[20px] border border-white/10 bg-black/18 p-4 shadow-[0_18px_54px_rgba(0,0,0,0.18)] backdrop-blur-md sm:p-5">
      <header className="mb-4 flex items-center justify-between">
        <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-slate-500">
          Pacing
        </h3>
        <button
          type="button"
          onClick={resetPacing}
          disabled={allAtDefault}
          className={`text-[0.62rem] font-semibold uppercase tracking-[0.18em] transition-colors ${
            allAtDefault
              ? "cursor-not-allowed text-slate-700"
              : "text-slate-500 hover:text-cyan-200"
          }`}
        >
          Reset section
        </button>
      </header>

      <div className="flex flex-col gap-5">
        <MultiplierSlider
          label="Animation Speed"
          description="Master speed — higher is faster. Full-right skips animations entirely."
          value={ANIMATION_SPEED_MAX - animationSpeedMultiplier}
          defaultValue={ANIMATION_SPEED_MAX - ANIMATION_SPEED_DEFAULT}
          min={ANIMATION_SPEED_MIN}
          max={ANIMATION_SPEED_MAX}
          step={ANIMATION_SPEED_STEP}
          onChange={(speed) => setAnimationSpeedMultiplier(ANIMATION_SPEED_MAX - speed)}
        />

        {PACING_CATEGORIES.map((category) => (
          <MultiplierSlider
            key={category}
            label={PACING_LABELS[category]}
            description={PACING_DESCRIPTIONS[category]}
            value={PACING_MAX - pacingMultipliers[category]}
            defaultValue={PACING_MAX - PACING_DEFAULT}
            min={PACING_MIN}
            max={PACING_MAX}
            step={PACING_STEP}
            onChange={(speed) => setPacingMultiplier(category, PACING_MAX - speed)}
          />
        ))}
      </div>

      <p className="mt-4 text-[0.68rem] leading-relaxed text-slate-500">
        Per-category sliders multiply on top of Animation Speed. Double-click any
        slider — or tap the <span className="text-slate-300">↺</span> next to it — to reset.
      </p>
    </section>
  );
}

function SegmentedControl<T extends string>({
  options,
  value,
  onChange,
}: {
  options: T[];
  value: T;
  onChange: (v: T) => void;
}) {
  return (
    <div className="flex min-h-11 flex-wrap rounded-[16px] border border-white/10 bg-black/18 p-1">
      {options.map((opt) => (
        <button
          key={opt}
          onClick={() => onChange(opt)}
          className={`min-h-9 flex-1 rounded-[12px] px-3 py-2 text-xs font-semibold capitalize transition-colors ${
            value === opt
              ? "bg-sky-500/80 text-white"
              : "text-slate-400 hover:text-slate-200"
          }`}
        >
          {opt}
        </button>
      ))}
    </div>
  );
}
