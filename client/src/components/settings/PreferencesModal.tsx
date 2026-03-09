import { AnimatePresence, motion } from "framer-motion";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { AnimationSpeed, VfxQuality } from "../../animation/types.ts";
import type {
  CardSizePreference,
  HudLayout,
  LogDefaultState,
  BoardBackground,
} from "../../stores/preferencesStore.ts";

interface PreferencesModalProps {
  onClose: () => void;
}

const CARD_SIZES: CardSizePreference[] = ["small", "medium", "large"];
const HUD_LAYOUTS: HudLayout[] = ["inline", "floating"];
const LOG_DEFAULTS: LogDefaultState[] = ["open", "closed"];
const VFX_QUALITIES: VfxQuality[] = ["full", "reduced", "minimal"];
const ANIMATION_SPEEDS: AnimationSpeed[] = ["slow", "normal", "fast", "instant"];
const BOARD_BACKGROUNDS: { value: BoardBackground; label: string }[] = [
  { value: "auto-wubrg", label: "Auto (WUBRG)" },
  { value: "white", label: "White" },
  { value: "blue", label: "Blue" },
  { value: "black", label: "Black" },
  { value: "red", label: "Red" },
  { value: "green", label: "Green" },
  { value: "none", label: "None" },
];

export function PreferencesModal({ onClose }: PreferencesModalProps) {
  const cardSize = usePreferencesStore((s) => s.cardSize);
  const hudLayout = usePreferencesStore((s) => s.hudLayout);
  const logDefaultState = usePreferencesStore((s) => s.logDefaultState);
  const boardBackground = usePreferencesStore((s) => s.boardBackground);
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const animationSpeed = usePreferencesStore((s) => s.animationSpeed);
  const setCardSize = usePreferencesStore((s) => s.setCardSize);
  const setHudLayout = usePreferencesStore((s) => s.setHudLayout);
  const setLogDefaultState = usePreferencesStore((s) => s.setLogDefaultState);
  const setBoardBackground = usePreferencesStore((s) => s.setBoardBackground);
  const setVfxQuality = usePreferencesStore((s) => s.setVfxQuality);
  const setAnimationSpeed = usePreferencesStore((s) => s.setAnimationSpeed);

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
          className="relative z-10 w-full max-w-sm rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700"
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

          <div className="flex flex-col gap-5">
            {/* Card Size */}
            <SettingGroup label="Card Size">
              <SegmentedControl
                options={CARD_SIZES}
                value={cardSize}
                onChange={setCardSize}
              />
            </SettingGroup>

            {/* HUD Layout */}
            <SettingGroup label="HUD Layout">
              <SegmentedControl
                options={HUD_LAYOUTS}
                value={hudLayout}
                onChange={setHudLayout}
              />
            </SettingGroup>

            {/* Log Default State */}
            <SettingGroup label="Log Default">
              <SegmentedControl
                options={LOG_DEFAULTS}
                value={logDefaultState}
                onChange={setLogDefaultState}
              />
            </SettingGroup>

            {/* Board Background */}
            <SettingGroup label="Board Background">
              <select
                value={boardBackground}
                onChange={(e) => setBoardBackground(e.target.value as BoardBackground)}
                className="w-full rounded bg-gray-800 px-3 py-1.5 text-sm text-gray-200 ring-1 ring-gray-700 focus:outline-none focus:ring-cyan-500"
              >
                {BOARD_BACKGROUNDS.map((bg) => (
                  <option key={bg.value} value={bg.value}>
                    {bg.label}
                  </option>
                ))}
              </select>
            </SettingGroup>

            {/* VFX Quality */}
            <SettingGroup label="VFX Quality">
              <SegmentedControl
                options={VFX_QUALITIES}
                value={vfxQuality}
                onChange={setVfxQuality}
              />
            </SettingGroup>

            {/* Animation Speed */}
            <SettingGroup label="Animation Speed">
              <SegmentedControl
                options={ANIMATION_SPEEDS}
                value={animationSpeed}
                onChange={setAnimationSpeed}
              />
            </SettingGroup>
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
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
