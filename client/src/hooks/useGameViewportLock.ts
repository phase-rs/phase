import { useEffect } from "react";

const GAME_VIEWPORT_LOCK_CLASS = "game-viewport-lock";

interface ViewportLockOwnership {
  owners: number;
  wasAlreadyLocked: boolean;
}

const viewportLockOwnership = new WeakMap<Element, ViewportLockOwnership>();

function acquireViewportLock(element: Element): () => void {
  const ownership = viewportLockOwnership.get(element);
  if (ownership) {
    ownership.owners += 1;
  } else {
    viewportLockOwnership.set(element, {
      owners: 1,
      wasAlreadyLocked: element.classList.contains(GAME_VIEWPORT_LOCK_CLASS),
    });
  }
  element.classList.add(GAME_VIEWPORT_LOCK_CLASS);

  return () => {
    const current = viewportLockOwnership.get(element);
    if (!current) return;
    if (current.owners > 1) {
      current.owners -= 1;
      return;
    }

    viewportLockOwnership.delete(element);
    if (!current.wasAlreadyLocked) {
      element.classList.remove(GAME_VIEWPORT_LOCK_CLASS);
    }
  };
}

/**
 * Locks the document viewport while the battlefield is mounted.
 *
 * The fixed document prevents iOS rubber-band scrolling from moving the PWA,
 * while nested overflow containers such as drawers and dialogs remain
 * independently scrollable.
 */
export function useGameViewportLock() {
  useEffect(() => {
    const releaseRoot = acquireViewportLock(document.documentElement);
    const releaseBody = acquireViewportLock(document.body);

    return () => {
      releaseRoot();
      releaseBody();
    };
  }, []);
}
