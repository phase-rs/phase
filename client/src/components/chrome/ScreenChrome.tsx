import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { menuButtonClass } from "../menu/buttonStyles";
import { ChromeControls } from "./ChromeControls";
import { useInShell } from "./ShellContext";

interface ScreenChromeProps {
  onBack?: () => void;
  settingsOpen?: boolean;
  onSettingsOpenChange?: (open: boolean) => void;
}

function BackIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 20 20"
      fill="currentColor"
      className="h-5 w-5"
      aria-hidden="true"
    >
      <path
        fillRule="evenodd"
        d="M17 10a.75.75 0 0 1-.75.75H5.56l3.22 3.22a.75.75 0 1 1-1.06 1.06l-4.5-4.5a.75.75 0 0 1 0-1.06l4.5-4.5a.75.75 0 0 1 1.06 1.06L5.56 9.25h10.69A.75.75 0 0 1 17 10Z"
        clipRule="evenodd"
      />
    </svg>
  );
}

/**
 * Full-page menu chrome: a back button (upper-left) plus the persistent control
 * cluster (volume / account / settings / fullscreen). Inside the modern app
 * shell the rail and the shell-level ChromeControls own all of this, so this
 * renders nothing — the shell is the single source of chrome.
 */
export function ScreenChrome({
  onBack,
  settingsOpen,
  onSettingsOpenChange,
}: ScreenChromeProps) {
  const { t } = useTranslation();

  // In the modern shell the rail (nav/branding) and the shell's ChromeControls
  // render the chrome exactly once; per-page ScreenChrome would duplicate it.
  if (useInShell()) return null;

  return (
    <>
      {onBack && (
        <div className="fixed left-4 top-[calc(env(safe-area-inset-top)+1rem)] z-30">
          <motion.button
            className={menuButtonClass({
              tone: "neutral",
              size: "chrome",
              className: "text-white/68 hover:text-white",
            })}
            whileHover={{ y: -1 }}
            whileTap={{ scale: 0.98 }}
            onClick={onBack}
            aria-label={t("chrome.back")}
            title={t("chrome.back")}
          >
            <BackIcon />
          </motion.button>
        </div>
      )}

      <ChromeControls
        settingsOpen={settingsOpen}
        onSettingsOpenChange={onSettingsOpenChange}
      />
    </>
  );
}
