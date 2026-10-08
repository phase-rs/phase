import { afterEach, describe, expect, it, vi } from "vitest";

import { copyText } from "../copyText";

const realClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");

function setClipboard(value: unknown): void {
  Object.defineProperty(navigator, "clipboard", { value, configurable: true });
}

function setExecCommand(impl: (() => boolean) | undefined): void {
  Object.defineProperty(document, "execCommand", { value: impl, configurable: true });
}

afterEach(() => {
  if (realClipboard) Object.defineProperty(navigator, "clipboard", realClipboard);
  else Reflect.deleteProperty(navigator, "clipboard");
  Reflect.deleteProperty(document, "execCommand");
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  vi.restoreAllMocks();
});

describe("copyText", () => {
  it("writes through the async clipboard when it works", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    setClipboard({ writeText });

    await expect(copyText("join-me")).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith("join-me");
  });

  // tauri#5835: the WebKitGTK webview can have no `navigator.clipboard` at all.
  it("falls back when the async clipboard is absent", async () => {
    setClipboard(undefined);
    const exec = vi.fn(() => true);
    setExecCommand(exec);

    await expect(copyText("join-me")).resolves.toBe(true);
    expect(exec).toHaveBeenCalledWith("copy");
  });

  // tauri#10835: present, but the write never lands.
  it("falls back when the async clipboard rejects", async () => {
    const writeText = vi.fn(() => Promise.reject(new Error("denied")));
    setClipboard({ writeText });
    const exec = vi.fn(() => true);
    setExecCommand(exec);

    await expect(copyText("join-me")).resolves.toBe(true);
    expect(writeText).toHaveBeenCalled();
    expect(exec).toHaveBeenCalledWith("copy");
  });

  it("reports false when neither path can write", async () => {
    setClipboard(undefined);
    setExecCommand(() => false);

    await expect(copyText("join-me")).resolves.toBe(false);
  });

  it("reports false when the fallback itself throws", async () => {
    setClipboard(undefined);
    setExecCommand(() => {
      throw new Error("unsupported");
    });

    await expect(copyText("join-me")).resolves.toBe(false);
  });

  // In the shell the write has to land while the click is still on the stack:
  // `execCommand` is gated on user activation, and an awaited rejection resumes
  // a task too late for that.
  it("copies synchronously inside the desktop shell", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { value: {}, configurable: true });
    const writeText = vi.fn(() => Promise.reject(new Error("denied")));
    setClipboard({ writeText });
    const exec = vi.fn(() => true);
    setExecCommand(exec);

    const pending = copyText("join-me");
    expect(exec).toHaveBeenCalledWith("copy");

    await expect(pending).resolves.toBe(true);
    expect(writeText).not.toHaveBeenCalled();
  });

  it("hands the text to the fallback and leaves no element behind", async () => {
    setClipboard(undefined);
    let selected: string | null = null;
    setExecCommand(() => {
      selected = document.querySelector("textarea")?.value ?? null;
      return true;
    });

    await expect(copyText("join-me")).resolves.toBe(true);
    expect(selected).toBe("join-me");
    expect(document.querySelector("textarea")).toBeNull();
  });
});
