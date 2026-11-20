import { act } from "react";
import { vi } from "vitest";

/**
 * A stand-in browser layout for titles fitted by `useBannerTitleFit`. happy-dom
 * lays nothing out and its ResizeObserver never fires, so a test sets the widths a
 * browser would report and reports resizes itself.
 *
 * The hook measures a title by the width it gives it: its line (`100%`), one
 * unbroken line (`max-content`) and its longest unbreakable run (`min-content`),
 * each at the title's size class. A title reports the matching width below;
 * every other element reports 0.
 */
export type TitleWidths = {
  line: number;
  oneLine: number;
  longestRun: number;
};

const measureByWidth: Record<string, keyof TitleWidths> = {
  "100%": "line",
  "max-content": "oneLine",
  "min-content": "longestRun",
};

/** Records what it observes and fires only when a test reports one of its targets resized. */
class ControlledResizeObserver implements ResizeObserver {
  static readonly instances = new Set<ControlledResizeObserver>();
  readonly observed = new Set<Element>();
  readonly callback: ResizeObserverCallback;

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    ControlledResizeObserver.instances.add(this);
  }

  observe(target: Element) {
    this.observed.add(target);
  }

  unobserve(target: Element) {
    this.observed.delete(target);
  }

  disconnect() {
    this.observed.clear();
  }
}

/**
 * Installs the stand-in layout. `widths` stays live: change it, then report a
 * resize or resolve the fonts to have the hook measure again. The root and the
 * size classes get their real font sizes (16px, and 48, 36 and 24px), which
 * happy-dom has no stylesheet for. Undo it with `vi.restoreAllMocks()`,
 * `vi.unstubAllGlobals()` and the returned `restore`.
 */
export function stubBannerTitleLayout(widths: TitleWidths) {
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (this: HTMLElement) {
    const measure = measureByWidth[this.style.width];
    return measure ? widths[measure] : 0;
  });

  const sizeClasses = document.createElement("style");
  sizeClasses.textContent =
    ".text-5xl { font-size: 48px; } .text-4xl { font-size: 36px; } .text-2xl { font-size: 24px; }";
  document.head.appendChild(sizeClasses);
  document.documentElement.style.fontSize = "16px";

  ControlledResizeObserver.instances.clear();
  vi.stubGlobal("ResizeObserver", ControlledResizeObserver);

  let resolveFonts: () => void = () => {};
  const ready = new Promise<void>((resolve) => {
    resolveFonts = () => resolve();
  });
  Object.defineProperty(document, "fonts", { configurable: true, value: { ready } });

  return {
    /** Notifies the observers watching `target`, as a browser does when it resizes. */
    reportResize(target: Element) {
      act(() => {
        for (const observer of ControlledResizeObserver.instances) {
          if (observer.observed.has(target)) {
            observer.callback([], observer);
          }
        }
      });
    },
    /** Runs the frame the hook schedules its refit in. */
    async nextFrame() {
      await act(async () => {
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      });
    },
    /** Finishes loading the page's fonts. */
    async loadFonts() {
      await act(async () => {
        resolveFonts();
        await Promise.resolve();
      });
    },
    restore() {
      sizeClasses.remove();
      document.documentElement.style.removeProperty("font-size");
      Reflect.deleteProperty(document, "fonts");
    },
  };
}
