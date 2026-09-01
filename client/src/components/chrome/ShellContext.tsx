import { createContext, useContext, useLayoutEffect, type ReactNode } from "react";

/**
 * True when a screen is rendered inside the modern AppShell (persistent rail +
 * tab bar). Menu pages read this to drop their own full-page chrome — the scene
 * backdrop, the floating particle canvas, and the ScreenChrome cluster — which
 * the shell now renders exactly once. Defaults to `false` so any screen rendered
 * outside the shell (e.g. the full-screen `/game/:id` route) keeps its own
 * chrome unchanged.
 */
const ShellContext = createContext(false);

export type DraftShellChromeMode =
  | "default"
  | "phone-drafting"
  | "phone-deckbuilding"
  | "tablet-drafting"
  | "tablet-deckbuilding";

export type DraftShellPhoneAction = {
  icon: ReactNode;
  label: string;
  onClick: () => void;
};

export type DraftShellTopAction = {
  id: "pause-resume" | "end-draft";
  label: string;
  tone: "neutral" | "emerald" | "danger";
  disabled?: boolean;
  onClick: () => void;
};

export type DraftShellProgressVariant = "quick" | "pod";

export type DraftShellChromeConfig = {
  mode: DraftShellChromeMode;
  phoneAction?: DraftShellPhoneAction;
  progressVariant?: DraftShellProgressVariant;
  showProgress?: boolean;
  topActions?: readonly DraftShellTopAction[];
};

const EMPTY_DRAFT_SHELL_TOP_ACTIONS: readonly DraftShellTopAction[] = [];

const DraftShellChromeContext = createContext<(config: DraftShellChromeConfig) => void>(
  () => undefined,
);

export const ShellProvider = ShellContext.Provider;
export const DraftShellChromeProvider = DraftShellChromeContext.Provider;

/** Hook: is the current screen embedded in the modern app shell? */
export function useInShell(): boolean {
  return useContext(ShellContext);
}

export function useDraftShellChrome(
  mode: DraftShellChromeMode,
  phoneAction?: DraftShellPhoneAction,
  progressVariant: DraftShellProgressVariant = "quick",
  showProgress = true,
  topActions: readonly DraftShellTopAction[] = EMPTY_DRAFT_SHELL_TOP_ACTIONS,
): void {
  const setConfig = useContext(DraftShellChromeContext);

  useLayoutEffect(() => {
    setConfig({ mode, phoneAction, progressVariant, showProgress, topActions });
    return () => setConfig({ mode: "default" });
  }, [mode, phoneAction, progressVariant, setConfig, showProgress, topActions]);
}
