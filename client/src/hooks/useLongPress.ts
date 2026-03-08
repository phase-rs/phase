import { useCallback, useRef } from "react";

interface UseLongPressOptions {
  delay?: number;
}

export function useLongPress(
  callback: () => void,
  options?: UseLongPressOptions,
) {
  const { delay = 500 } = options ?? {};
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clear = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const onTouchStart = useCallback(() => {
    timerRef.current = setTimeout(callback, delay);
  }, [callback, delay]);

  const onTouchEnd = useCallback(() => {
    clear();
  }, [clear]);

  const onTouchCancel = useCallback(() => {
    clear();
  }, [clear]);

  return { onTouchStart, onTouchEnd, onTouchCancel };
}
