import { useState } from "react";
import { motion } from "framer-motion";

import { menuButtonClass } from "../menu/buttonStyles";
import { PreferencesModal } from "../settings/PreferencesModal";

interface ScreenChromeProps {
  onBack?: () => void;
  showLogo?: boolean;
}

function BackIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 20 20"
      fill="currentColor"
      className="h-6 w-6"
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

function SettingsIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 20 20"
      fill="currentColor"
      className="w-6 h-6"
    >
      <path
        fillRule="evenodd"
        d="M7.84 1.804A1 1 0 0 1 8.82 1h2.36a1 1 0 0 1 .98.804l.331 1.652a6.993 6.993 0 0 1 1.929 1.115l1.598-.54a1 1 0 0 1 1.186.447l1.18 2.044a1 1 0 0 1-.205 1.251l-1.267 1.113a7.047 7.047 0 0 1 0 2.228l1.267 1.113a1 1 0 0 1 .206 1.25l-1.18 2.045a1 1 0 0 1-1.187.447l-1.598-.54a6.993 6.993 0 0 1-1.929 1.115l-.33 1.652a1 1 0 0 1-.98.804H8.82a1 1 0 0 1-.98-.804l-.331-1.652a6.993 6.993 0 0 1-1.929-1.115l-1.598.54a1 1 0 0 1-1.186-.447l-1.18-2.044a1 1 0 0 1 .205-1.251l1.267-1.114a7.05 7.05 0 0 1 0-2.227L1.821 7.773a1 1 0 0 1-.206-1.25l1.18-2.045a1 1 0 0 1 1.187-.447l1.598.54A6.992 6.992 0 0 1 7.51 3.456l.33-1.652ZM10 13a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z"
        clipRule="evenodd"
      />
    </svg>
  );
}

export function ScreenChrome({ onBack, showLogo = false }: ScreenChromeProps) {
  const [showSettings, setShowSettings] = useState(false);

  return (
    <>
      {/* Back button — upper-left */}
      {onBack && (
        <div className="fixed left-4 top-[calc(env(safe-area-inset-top)+1rem)] z-30">
          <motion.button
            className={menuButtonClass({
              tone: "slate",
              size: "sm",
              className:
                "w-12 h-12 p-0 rounded-full flex items-center justify-center text-white/70 hover:text-white",
            })}
            whileHover={{ scale: 1.05 }}
            whileTap={{ scale: 0.95 }}
            onClick={onBack}
            aria-label="Back"
            title="Back"
          >
            <BackIcon />
          </motion.button>
        </div>
      )}

      {/* Logo — top-center */}
      {showLogo && (
        <div className="fixed top-[calc(env(safe-area-inset-top)+0.75rem)] left-1/2 -translate-x-1/2 z-20 pointer-events-none">
          <img
            src="/logo.webp"
            alt="phase.rs"
            className="w-48 max-w-[40vw]"
            style={{
              filter:
                "drop-shadow(0 0 20px rgba(251, 146, 60, 0.45)) drop-shadow(0 0 45px rgba(251, 146, 60, 0.2))",
            }}
          />
        </div>
      )}

      {/* Settings cog — upper-right */}
      <div className="fixed right-4 top-[calc(env(safe-area-inset-top)+1rem)] z-30">
        <motion.button
          className={menuButtonClass({
            tone: "slate",
            size: "sm",
            className:
              "w-12 h-12 p-0 rounded-full flex items-center justify-center text-white/40 hover:text-white/70",
          })}
          whileHover={{ scale: 1.05 }}
          whileTap={{ scale: 0.95 }}
          onClick={() => setShowSettings(true)}
          aria-label="Settings"
          title="Settings"
        >
          <SettingsIcon />
        </motion.button>
      </div>

      {/* Settings modal */}
      {showSettings && (
        <PreferencesModal onClose={() => setShowSettings(false)} />
      )}
    </>
  );
}
