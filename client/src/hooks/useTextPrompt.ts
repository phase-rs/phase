import { useCallback, useRef, useState } from "react";

/** Promise-backed TextPromptDialog state — open/confirm/cancel without window.prompt(). */
export function useTextPrompt() {
  const resolverRef = useRef<((value: string | null) => void) | null>(null);
  const [open, setOpen] = useState(false);

  const request = useCallback(() => {
    return new Promise<string | null>((resolve) => {
      resolverRef.current = resolve;
      setOpen(true);
    });
  }, []);

  const confirm = useCallback((value: string) => {
    setOpen(false);
    resolverRef.current?.(value);
    resolverRef.current = null;
  }, []);

  const cancel = useCallback(() => {
    setOpen(false);
    resolverRef.current?.(null);
    resolverRef.current = null;
  }, []);

  return { open, request, confirm, cancel };
}
