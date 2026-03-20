import { useState } from "react";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import type { AnimationSpeed, CombatPacing, VfxQuality } from "../../animation/types.ts";
import type {
  CardSizePreference,
  LogDefaultState,
} from "../../stores/preferencesStore.ts";
import { BATTLEFIELDS } from "../board/battlefields.ts";
import { ModalPanelShell } from "../ui/ModalPanelShell";

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
  { value: "random", label: "Random" },
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

                  <SettingGroup label="Board Background">
                    <select
                      value={boardBackground}
                      onChange={(e) => setBoardBackground(e.target.value)}
                      className="w-full rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 focus:border-sky-400/40 focus:outline-none"
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
                  <p className="text-xs text-slate-500">
                    Controls the pause before damage after blockers and between combat engagements.
                  </p>
                </SettingsSection>
              )}

              {activeTab === "audio" && (
                <SettingsSection title="Audio">
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
                    <div className={`flex flex-col gap-2 sm:flex-row sm:items-center ${sfxMuted ? "opacity-50" : ""}`}>
                      <label className="flex min-h-11 items-center gap-2">
                        <input
                          type="checkbox"
                          checked={sfxMuted}
                          onChange={(e) => setSfxMuted(e.target.checked)}
                          className="accent-cyan-500"
                        />
                        <span className="text-xs text-slate-400">Mute</span>
                      </label>
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
                    <div className={`flex flex-col gap-2 sm:flex-row sm:items-center ${musicMuted ? "opacity-50" : ""}`}>
                      <label className="flex min-h-11 items-center gap-2">
                        <input
                          type="checkbox"
                          checked={musicMuted}
                          onChange={(e) => setMusicMuted(e.target.checked)}
                          className="accent-cyan-500"
                        />
                        <span className="text-xs text-slate-400">Mute</span>
                      </label>
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
                        className="w-full rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 placeholder-slate-500 focus:border-sky-400/40 focus:outline-none"
                      />
                  </SettingGroup>

                  <SettingGroup label="Server Address">
                    <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
                      <input
                        type="text"
                        value={serverAddress}
                        onChange={(e) => setServerAddress(e.target.value)}
                        placeholder="ws://localhost:9374/ws"
                        className="min-h-11 flex-1 rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 placeholder-slate-500 focus:border-sky-400/40 focus:outline-none"
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
                        className="min-h-11 rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-xs font-semibold uppercase tracking-[0.18em] text-slate-200 transition hover:bg-white/6 sm:self-auto"
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
    </ModalPanelShell>
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
