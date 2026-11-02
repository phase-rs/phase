import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DOCUMENT_HIDDEN_ATTRIBUTE, installHiddenDocumentMotion } from "../hiddenDocumentMotion";

let visibilityState: DocumentVisibilityState = "visible";

function setVisibility(next: DocumentVisibilityState) {
  visibilityState = next;
  document.dispatchEvent(new Event("visibilitychange"));
}

const hidden = () => document.documentElement.hasAttribute(DOCUMENT_HIDDEN_ATTRIBUTE);

describe("installHiddenDocumentMotion", () => {
  let dispose: (() => void) | undefined;

  beforeEach(() => {
    visibilityState = "visible";
    vi.spyOn(document, "visibilityState", "get").mockImplementation(() => visibilityState);
  });

  afterEach(() => {
    dispose?.();
    dispose = undefined;
    vi.restoreAllMocks();
  });

  it("marks the document only while it is hidden", () => {
    dispose = installHiddenDocumentMotion(document);
    expect(hidden()).toBe(false);

    setVisibility("hidden");
    expect(hidden()).toBe(true);

    setVisibility("visible");
    expect(hidden()).toBe(false);
  });

  it("marks a document that is already hidden when installed", () => {
    visibilityState = "hidden";
    dispose = installHiddenDocumentMotion(document);

    expect(hidden()).toBe(true);
  });

  it("clears the mark and stops listening when disposed", () => {
    dispose = installHiddenDocumentMotion(document);
    setVisibility("hidden");

    dispose();
    dispose = undefined;
    expect(hidden()).toBe(false);

    setVisibility("hidden");
    expect(hidden()).toBe(false);
  });

  it("ignores the disposer of an installation that was since replaced", () => {
    setVisibility("hidden");
    const disposeReplaced = installHiddenDocumentMotion(document);
    dispose = installHiddenDocumentMotion(document);

    disposeReplaced();
    expect(hidden()).toBe(true);
  });

  it("replaces an earlier installation instead of stacking listeners", () => {
    installHiddenDocumentMotion(document);
    dispose = installHiddenDocumentMotion(document);
    setVisibility("hidden");

    dispose();
    dispose = undefined;
    setVisibility("hidden");
    expect(hidden()).toBe(false);
  });
});
