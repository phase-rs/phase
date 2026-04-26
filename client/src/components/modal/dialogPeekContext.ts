import { createContext, useContext } from "react";

export interface DialogPeekContext {
  peeked: boolean;
  togglePeek: () => void;
  setPeeked: (value: boolean) => void;
}

export const DialogPeekCtx = createContext<DialogPeekContext | null>(null);

export function useOptionalDialogPeek(): DialogPeekContext | null {
  return useContext(DialogPeekCtx);
}

export function useDialogPeek(): DialogPeekContext {
  const ctx = useContext(DialogPeekCtx);
  if (!ctx) {
    throw new Error("useDialogPeek must be used inside a DialogHost");
  }
  return ctx;
}
