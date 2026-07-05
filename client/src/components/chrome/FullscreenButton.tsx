import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

function EnterFullscreenIcon({ className }: { className?: string }) {
  return (
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className={className ?? "w-5 h-5"}>
      <path d="M3.75 3A.75.75 0 0 0 3 3.75v2.5a.75.75 0 0 0 1.5 0V4.5h1.75a.75.75 0 0 0 0-1.5h-2.5ZM13.75 3a.75.75 0 0 0 0 1.5h1.75v1.75a.75.75 0 0 0 1.5 0v-2.5a.75.75 0 0 0-.75-.75h-2.5ZM3 13.75a.75.75 0 0 1 1.5 0v1.75h1.75a.75.75 0 0 1 0 1.5h-2.5a.75.75 0 0 1-.75-.75v-2.5ZM16.5 13.75a.75.75 0 0 0-1.5 0v1.75h-1.75a.75.75 0 0 0 0 1.5h2.5a.75.75 0 0 0 .75-.75v-2.5Z" />
    </svg>
  );
}

function ExitFullscreenIcon({ className }: { className?: string }) {
  return (
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className={className ?? "w-5 h-5"}>
      <path d="M3.28 2.22a.75.75 0 0 0-1.06 1.06L5.44 6.5H3.75a.75.75 0 0 0 0 1.5h3.5a.75.75 0 0 0 .75-.75v-3.5a.75.75 0 0 0-1.5 0v1.69L3.28 2.22ZM16.72 2.22a.75.75 0 0 1 1.06 1.06L14.56 6.5h1.69a.75.75 0 0 1 0 1.5h-3.5a.75.75 0 0 1-.75-.75v-3.5a.75.75 0 0 1 1.5 0v1.69l3.22-3.22ZM3.28 17.78a.75.75 0 0 1-1.06-1.06L5.44 13.5H3.75a.75.75 0 0 1 0-1.5h3.5a.75.75 0 0 1 .75.75v3.5a.75.75 0 0 1-1.5 0v-1.69l-3.22 3.22ZM16.72 17.78a.75.75 0 0 0 1.06-1.06L14.56 13.5h1.69a.75.75 0 0 0 0-1.5h-3.5a.75.75 0 0 0-.75.75v3.5a.75.75 0 0 0 1.5 0v-1.69l3.22 3.22Z" />
    </svg>
  );
}

interface FullscreenButtonProps {
  variant: "chrome" | "game";
}

export function FullscreenButton({ variant }: FullscreenButtonProps) {
  const { t } = useTranslation();
  const [isFullscreen, setIsFullscreen] = useState(!!document.fullscreenElement);

  useEffect(() => {
    function onChange() {
      setIsFullscreen(!!document.fullscreenElement);
    }
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  }, []);

  const toggle = useCallback(() => {
    if (document.fullscreenElement) {
      document.exitFullscreen();
    } else {
      document.documentElement.requestFullscreen();
    }
  }, []);

  const Icon = isFullscreen ? ExitFullscreenIcon : EnterFullscreenIcon;
  const label = isFullscreen ? t("fullscreen.exit") : t("fullscreen.enter");

  if (variant === "game") {
    return (
      <button
        onClick={toggle}
        className="flex h-7 w-7 items-center justify-center rounded-md bg-white/6 text-gray-400 transition-colors hover:bg-white/10 hover:text-gray-200"
        aria-label={label}
        title={label}
      >
        <Icon className="h-4 w-4" />
      </button>
    );
  }

  return (
    <button
      onClick={toggle}
      className="flex min-h-9 min-w-9 items-center justify-center rounded-[12px] border border-white/12 bg-black/18 text-white/46 backdrop-blur-sm transition-colors hover:text-white/72"
      aria-label={label}
      title={label}
    >
      <Icon />
    </button>
  );
}
