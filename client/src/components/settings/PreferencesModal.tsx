import { useState } from "react";
import { AnimatePresence, motion } from "framer-motion";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import type { AnimationSpeed, CombatPacing, VfxQuality } from "../../animation/types.ts";
import type {
  CardSizePreference,
  LogDefaultState,
} from "../../stores/preferencesStore.ts";
import { BATTLEFIELDS } from "../board/battlefields.ts";

interface PreferencesModalProps {
  onClose: () => void;
}

const CARD_SIZES: CardSizePreference[] = ["small", "medium", "large"];
const LOG_DEFAULTS: LogDefaultState[] = ["open", "closed"];
const VFX_QUALITIES: VfxQuality[] = ["full", "reduced", "minimal"];
const ANIMATION_SPEEDS: AnimationSpeed[] = ["slow", "normal", "fast", "instant"];
const COMBAT_PACINGS: CombatPacing[] = ["normal", "slow", "cinematic"];
const SETTINGS_TABS = [
  { id: "gameplay", label: "Gameplay" },
  { id: "visual", label: "Visual" },
  { id: "combat", label: "Combat" },
  { id: "audio", label: "Audio" },
  { id: "multiplayer", label: "Multiplayer" },
] as const;

type SettingsTabId = (typeof SETTINGS_TABS)[number]["id"];

const BOARD_BACKGROUNDS: { value: string; label: string }[] = [
  { value: "auto-wubrg", label: "Auto (match deck)" },
  ...BATTLEFIELDS.map((bf) => ({ value: bf.id, label: `${bf.label} (${bf.color})` })),
  { value: "none", label: "None" },
];

export function PreferencesModal({ onClose }: PreferencesModalProps) {
  const cardSize = usePreferencesStore((s) => s.cardSize);
  const logDefaultState = usePreferencesStore((s) => s.logDefaultState);
  const boardBackground = usePreferencesStore((s) => s.boardBackground);
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const animationSpeed = usePreferencesStore((s) => s.animationSpeed);
  const combatPacing = usePreferencesStore((s) => s.combatPacing);
  const setCardSize = usePreferencesStore((s) => s.setCardSize);
  const setLogDefaultState = usePreferencesStore((s) => s.setLogDefaultState);
  const setBoardBackground = usePreferencesStore((s) => s.setBoardBackground);
  const setVfxQuality = usePreferencesStore((s) => s.setVfxQuality);
  const setCombatPacing = usePreferencesStore((s) => s.setCombatPacing);
  const masterVolume = usePreferencesStore((s) => s.masterVolume);
  const sfxVolume = usePreferencesStore((s) => s.sfxVolume);
  const musicVolume = usePreferencesStore((s) => s.musicVolume);
  const sfxMuted = usePreferencesStore((s) => s.sfxMuted);
  const musicMuted = usePreferencesStore((s) => s.musicMuted);
  const setMasterVolume = usePreferencesStore((s) => s.setMasterVolume);
  const setSfxVolume = usePreferencesStore((s) => s.setSfxVolume);
  const setMusicVolume = usePreferencesStore((s) => s.setMusicVolume);
  const setSfxMuted = usePreferencesStore((s) => s.setSfxMuted);
  const setMusicMuted = usePreferencesStore((s) => s.setMusicMuted);
  const setAnimationSpeed = usePreferencesStore((s) => s.setAnimationSpeed);

  // Multiplayer settings
  const displayName = useMultiplayerStore((s) => s.displayName);
  const serverAddress = useMultiplayerStore((s) => s.serverAddress);
  const setDisplayName = useMultiplayerStore((s) => s.setDisplayName);
  const setServerAddress = useMultiplayerStore((s) => s.setServerAddress);
  const [activeTab, setActiveTab] = useState<SettingsTabId>("gameplay");
  const [connTest, setConnTest] = useState<"idle" | "testing" | "ok" | "fail">("idle");

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-0 z-50 flex items-center justify-center"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
        {/* Backdrop */}
        <div className="absolute inset-0 bg-black/60" onClick={onClose} />

        {/* Modal content */}
        <motion.div
          className="relative z-10 max-h-[90vh] w-full max-w-2xl overflow-y-auto rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700"
          initial={{ scale: 0.9, opacity: 0 }}
          animate={{ scale: 1, opacity: 1 }}
          exit={{ scale: 0.9, opacity: 0 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          {/* Header */}
          <div className="mb-5 flex items-center justify-between">
            <h2 className="text-lg font-bold text-white">Preferences</h2>
            <button
              onClick={onClose}
              className="rounded p-1 text-gray-500 transition-colors hover:bg-gray-800 hover:text-gray-300"
              aria-label="Close preferences"
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-5 w-5">
                <path d="M6.28 5.22a.75.75 0 0 0-1.06 1.06L8.94 10l-3.72 3.72a.75.75 0 1 0 1.06 1.06L10 11.06l3.72 3.72a.75.75 0 1 0 1.06-1.06L11.06 10l3.72-3.72a.75.75 0 0 0-1.06-1.06L10 8.94 6.28 5.22Z" />
              </svg>
            </button>
          </div>

          <div className="grid gap-4 md:grid-cols-[180px_minmax(0,1fr)]">
            <nav className="flex gap-2 overflow-x-auto pb-1 md:flex-col md:overflow-visible md:pb-0">
              {SETTINGS_TABS.map((tab) => (
                <button
                  key={tab.id}
                  onClick={() => setActiveTab(tab.id)}
                  className={`shrink-0 rounded-md border px-3 py-2 text-left text-xs font-semibold uppercase tracking-wide transition-colors md:w-full ${
                    activeTab === tab.id
                      ? "border-cyan-500/80 bg-cyan-600/20 text-cyan-200"
                      : "border-gray-800 bg-gray-950/50 text-gray-400 hover:border-gray-700 hover:text-gray-200"
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

                  <SettingGroup label="Board Background">
                    <select
                      value={boardBackground}
                      onChange={(e) => setBoardBackground(e.target.value)}
                      className="w-full rounded bg-gray-800 px-3 py-1.5 text-sm text-gray-200 ring-1 ring-gray-700 focus:outline-none focus:ring-cyan-500"
                    >
                      {BOARD_BACKGROUNDS.map((bg) => (
                        <option key={bg.value} value={bg.value}>
                          {bg.label}
                        </option>
                      ))}
                    </select>
                  </SettingGroup>
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

                  <SettingGroup label="Animation Speed">
                    <SegmentedControl
                      options={ANIMATION_SPEEDS}
                      value={animationSpeed}
                      onChange={setAnimationSpeed}
                    />
                  </SettingGroup>
                </SettingsSection>
              )}

              {activeTab === "combat" && (
                <SettingsSection title="Combat">
                  <SettingGroup label="Combat Pacing">
                    <SegmentedControl
                      options={COMBAT_PACINGS}
                      value={combatPacing}
                      onChange={setCombatPacing}
                    />
                  </SettingGroup>
                  <p className="text-xs text-gray-500">
                    Controls the pause before damage after blockers and between combat engagements.
                  </p>
                </SettingsSection>
              )}

              {activeTab === "audio" && (
                <SettingsSection title="Audio">
                  <SettingGroup label="Global Volume">
                    <div className="flex items-center gap-2">
                      <input
                        type="range"
                        min={0}
                        max={100}
                        value={masterVolume}
                        onChange={(e) => setMasterVolume(Number(e.target.value))}
                        className="flex-1 accent-cyan-500"
                      />
                      <span className="w-10 text-right text-xs text-gray-400">{masterVolume}%</span>
                    </div>
                  </SettingGroup>

                  <SettingGroup label="SFX Volume">
                    <div className={`flex items-center gap-2 ${sfxMuted ? "opacity-50" : ""}`}>
                      <label className="flex items-center gap-1">
                        <input
                          type="checkbox"
                          checked={sfxMuted}
                          onChange={(e) => setSfxMuted(e.target.checked)}
                          className="accent-cyan-500"
                        />
                        <span className="text-xs text-gray-400">Mute</span>
                      </label>
                      <input
                        type="range"
                        min={0}
                        max={100}
                        value={sfxVolume}
                        onChange={(e) => setSfxVolume(Number(e.target.value))}
                        className="flex-1 accent-cyan-500"
                      />
                      <span className="w-10 text-right text-xs text-gray-400">{sfxVolume}%</span>
                    </div>
                  </SettingGroup>

                  <SettingGroup label="Music Volume">
                    <div className={`flex items-center gap-2 ${musicMuted ? "opacity-50" : ""}`}>
                      <label className="flex items-center gap-1">
                        <input
                          type="checkbox"
                          checked={musicMuted}
                          onChange={(e) => setMusicMuted(e.target.checked)}
                          className="accent-cyan-500"
                        />
                        <span className="text-xs text-gray-400">Mute</span>
                      </label>
                      <input
                        type="range"
                        min={0}
                        max={100}
                        value={musicVolume}
                        onChange={(e) => setMusicVolume(Number(e.target.value))}
                        className="flex-1 accent-cyan-500"
                      />
                      <span className="w-10 text-right text-xs text-gray-400">{musicVolume}%</span>
                    </div>
                  </SettingGroup>
                </SettingsSection>
              )}

              {activeTab === "multiplayer" && (
                <SettingsSection title="Multiplayer">
                  <SettingGroup label="Display Name">
                    <input
                      type="text"
                      value={displayName}
                      onChange={(e) => setDisplayName(e.target.value)}
                      placeholder="Enter your name"
                      maxLength={20}
                      className="w-full rounded bg-gray-800 px-3 py-1.5 text-sm text-gray-200 ring-1 ring-gray-700 focus:outline-none focus:ring-cyan-500"
                    />
                  </SettingGroup>

                  <SettingGroup label="Server Address">
                    <div className="flex items-center gap-2">
                      <input
                        type="text"
                        value={serverAddress}
                        onChange={(e) => setServerAddress(e.target.value)}
                        placeholder="ws://localhost:9374/ws"
                        className="flex-1 rounded bg-gray-800 px-3 py-1.5 text-sm text-gray-200 ring-1 ring-gray-700 focus:outline-none focus:ring-cyan-500"
                      />
                      <button
                        onClick={() => {
                          setConnTest("testing");
                          const ws = new WebSocket(serverAddress);
                          const timeout = setTimeout(() => {
                            ws.close();
                            setConnTest("fail");
                          }, 3000);
                          ws.onopen = () => {
                            clearTimeout(timeout);
                            ws.close();
                            setConnTest("ok");
                          };
                          ws.onerror = () => {
                            clearTimeout(timeout);
                            setConnTest("fail");
                          };
                        }}
                        className="rounded bg-gray-700 px-2 py-1 text-xs text-gray-300 transition-colors hover:bg-gray-600"
                      >
                        Test
                      </button>
                    </div>
                    {connTest === "ok" && (
                      <p className="mt-1 text-xs text-emerald-400">Connected</p>
                    )}
                    {connTest === "fail" && (
                      <p className="mt-1 text-xs text-red-400">Connection failed</p>
                    )}
                    {connTest === "testing" && (
                      <p className="mt-1 text-xs text-gray-400">Testing...</p>
                    )}
                  </SettingGroup>
                </SettingsSection>
              )}
            </div>
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
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
    <section className="rounded-lg border border-gray-800 bg-gray-950/50 p-4">
      <h3 className="mb-3 text-sm font-semibold uppercase tracking-wide text-gray-300">{title}</h3>
      <div className="flex flex-col gap-4">{children}</div>
    </section>
  );
}

function SettingGroup({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="mb-1.5 block text-xs font-medium uppercase tracking-wider text-gray-400">
        {label}
      </label>
      {children}
    </div>
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
    <div className="flex rounded bg-gray-800 p-0.5 ring-1 ring-gray-700">
      {options.map((opt) => (
        <button
          key={opt}
          onClick={() => onChange(opt)}
          className={`flex-1 rounded px-3 py-1 text-xs font-medium capitalize transition-colors ${
            value === opt
              ? "bg-cyan-600 text-white"
              : "text-gray-400 hover:text-gray-200"
          }`}
        >
          {opt}
        </button>
      ))}
    </div>
  );
}
