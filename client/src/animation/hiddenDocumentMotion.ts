/**
 * Attribute set on `<html>` while the document is hidden. `index.css` keys a
 * rule off it that turns every CSS keyframe animation and transition off.
 */
export const DOCUMENT_HIDDEN_ATTRIBUTE = "data-document-hidden";

let disposeInstalled: (() => void) | null = null;

/**
 * A hidden tab renders no frames, while the game keeps updating the board (AI
 * seats, opponents, remote updates). When something forces style resolution
 * while hidden — framer-motion layout measurement does, every revision —
 * Chromium creates a CSSAnimation / CSSTransition for each element with an
 * `animate-*` or `transition-*` class and holds it until the next frame.
 * Unmounting the element does not release it, so every waiting-reason pill
 * (its `animate-pulse` dot), card frame, … that mounts and unmounts while
 * hidden stays in memory as detached DOM until the tab is shown again.
 *
 * Keeps {@link DOCUMENT_HIDDEN_ATTRIBUTE} on `<html>` in step with the
 * document's visibility, so no CSS animation or transition is created while
 * hidden; they start again when the tab is shown. Returns a disposer;
 * installing twice replaces the earlier installation.
 */
export function installHiddenDocumentMotion(doc: Document = document): () => void {
  disposeInstalled?.();

  const root = doc.documentElement;
  const sync = () => {
    root.toggleAttribute(DOCUMENT_HIDDEN_ATTRIBUTE, doc.visibilityState === "hidden");
  };

  doc.addEventListener("visibilitychange", sync);
  sync();

  const dispose = () => {
    // A replaced or already-disposed installation must not clear the mark
    // its successor owns.
    if (disposeInstalled !== dispose) return;
    disposeInstalled = null;
    doc.removeEventListener("visibilitychange", sync);
    root.removeAttribute(DOCUMENT_HIDDEN_ATTRIBUTE);
  };
  disposeInstalled = dispose;
  return dispose;
}
