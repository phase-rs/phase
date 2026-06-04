import { useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { usePreferencesStore } from "../../stores/preferencesStore";
import { menuButtonClass } from "../menu/buttonStyles";
import { PreferencesModal } from "../settings/PreferencesModal";
import { LanguageFlag } from "../ui/LanguageFlag";
import { AccountControl } from "./AccountControl";
import { FullscreenButton } from "./FullscreenButton";
import { VolumeControl } from "./VolumeControl";

interface ChromeControlsProps {
  /** Controlled settings-modal state (e.g. board context-menu deep-link). */
  settingsOpen?: boolean;
  onSettingsOpenChange?: (open: boolean) => void;
}

function SettingsIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 20 20"
      fill="currentColor"
      className="h-5 w-5"
    >
      <path
        fillRule="evenodd"
        d="M7.84 1.804A1 1 0 0 1 8.82 1h2.36a1 1 0 0 1 .98.804l.331 1.652a6.993 6.993 0 0 1 1.929 1.115l1.598-.54a1 1 0 0 1 1.186.447l1.18 2.044a1 1 0 0 1-.205 1.251l-1.267 1.113a7.047 7.047 0 0 1 0 2.228l1.267 1.113a1 1 0 0 1 .206 1.25l-1.18 2.045a1 1 0 0 1-1.187.447l-1.598-.54a6.993 6.993 0 0 1-1.929 1.115l-.33 1.652a1 1 0 0 1-.98.804H8.82a1 1 0 0 1-.98-.804l-.331-1.652a6.993 6.993 0 0 1-1.929-1.115l-1.598.54a1 1 0 0 1-1.186-.447l-1.18-2.044a1 1 0 0 1 .205-1.251l1.267-1.114a7.05 7.05 0 0 1 0-2.227L1.821 7.773a1 1 0 0 1-.206-1.25l1.18-2.045a1 1 0 0 1 1.187-.447l1.598.54A6.992 6.992 0 0 1 7.51 3.456l.33-1.652Z"
        clipRule="evenodd"
      />
    </svg>
  );
}

/**
 * The app's persistent control cluster — volume, account/cloud-sync, language +
 * settings, and (lower-right) fullscreen — plus the PreferencesModal it opens.
 * Extracted from ScreenChrome so the modern AppShell can render it exactly once
 * at app level while ScreenChrome still composes it for any non-shell screen.
 */
export function ChromeControls({
  settingsOpen,
  onSettingsOpenChange,
}: ChromeControlsProps) {
  const { t } = useTranslation();
  const language = usePreferencesStore((s) => s.language);
  const [internalShowSettings, setInternalShowSettings] = useState(false);
  const isSettingsControlled = settingsOpen !== undefined;
  const showSettings = isSettingsControlled ? settingsOpen : internalShowSettings;

  const setShowSettings = (open: boolean) => {
    if (!isSettingsControlled) setInternalShowSettings(open);
    onSettingsOpenChange?.(open);
  };

  return (
    <>
      {/* Account + Volume + Settings — upper-right. Fullscreen lives lower-right
          (below) to keep this cluster uncrowded. */}
      <div className="fixed right-4 top-[calc(env(safe-area-inset-top)+1rem)] z-40 flex gap-2">
        <VolumeControl variant="chrome" />
        <AccountControl />
        <motion.button
          className={menuButtonClass({
            tone: "neutral",
            size: "chrome",
            className: "text-white/46 hover:text-white/72",
          })}
          whileHover={{ y: -1 }}
          whileTap={{ scale: 0.98 }}
          onClick={() => setShowSettings(true)}
          aria-label={t("chrome.languageSettings", { lang: language.toUpperCase() })}
          title={t("chrome.languageTitle", { lang: language.toUpperCase() })}
        >
          <LanguageFlag lng={language} className="h-3.5 w-5 rounded-sm" />
        </motion.button>
        <motion.button
          className={menuButtonClass({
            tone: "neutral",
            size: "chrome",
            className: "text-white/46 hover:text-white/72",
          })}
          whileHover={{ y: -1 }}
          whileTap={{ scale: 0.98 }}
          onClick={() => setShowSettings(true)}
          aria-label={t("chrome.settings")}
          title={t("chrome.settings")}
        >
          <SettingsIcon />
        </motion.button>
      </div>

      {/* Fullscreen — lower-right, separated from the top cluster */}
      <div className="fixed right-4 bottom-[calc(env(safe-area-inset-bottom)+1rem)] z-40">
        <FullscreenButton variant="chrome" />
      </div>

      {showSettings && <PreferencesModal onClose={() => setShowSettings(false)} />}
    </>
  );
}
