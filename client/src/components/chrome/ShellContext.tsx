import { createContext, useContext } from "react";

/**
 * True when a screen is rendered inside the modern AppShell (persistent rail +
 * tab bar). Menu pages read this to drop their own full-page chrome — the scene
 * backdrop, the floating particle canvas, and the ScreenChrome cluster — which
 * the shell now renders exactly once. Defaults to `false` so any screen rendered
 * outside the shell (e.g. the full-screen `/game/:id` route) keeps its own
 * chrome unchanged.
 */
const ShellContext = createContext(false);

export const ShellProvider = ShellContext.Provider;

/** Hook: is the current screen embedded in the modern app shell? */
export function useInShell(): boolean {
  return useContext(ShellContext);
}
