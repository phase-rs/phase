import { useEffect, useLayoutEffect, useMemo, useRef } from "react";

/** Whether the modal session an import started in was still open when the import finished. Closing the modal ends a session; reopening starts a new one. */
export type ImportSession = "open" | "dismissed";

/** `begin` when an import request starts; `stateOf` after its awaits. */
export function useImportSession(open: boolean) {
  const session = useRef(0);
  useLayoutEffect(() => {
    session.current += 1;
  }, [open]);
  // The modal's own `open` toggling can't invalidate a session once the
  // component holding this ref has unmounted (e.g. the surface that renders
  // it navigated away), so an unmount ends every session that is still open.
  useEffect(() => {
    return () => {
      session.current += 1;
    };
  }, []);
  return useMemo(
    () => ({
      begin: () => session.current,
      stateOf: (started: number): ImportSession => (started === session.current ? "open" : "dismissed"),
    }),
    [],
  );
}
