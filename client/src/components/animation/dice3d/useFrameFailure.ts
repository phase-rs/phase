import { useCallback, useState } from "react";

/**
 * A throw inside a `requestAnimationFrame` callback happens outside React's
 * render, so no error boundary sees it and the renderer simply freezes. The
 * returned reporter rethrows such an error during the caller's next render,
 * where the nearest boundary can catch it and show its fallback.
 */
export function useFrameFailure(): (error: unknown) => void {
  const [, setFailure] = useState<null>(null);
  return useCallback((error: unknown) => {
    setFailure(() => {
      throw error;
    });
  }, []);
}
