import { isTauri } from "./platform";

/**
 * Copy `text` to the clipboard, reporting whether the write actually happened.
 * Callers that assume success paint a "copied" confirmation over a clipboard
 * that never changed, so the result is returned rather than thrown away.
 *
 * Inside the desktop shell the synchronous path goes first. The WebKitGTK
 * webview Tauri uses on Linux may have no `navigator.clipboard` at all
 * (tauri#5835), and when it has one the write can still fail — but `await`ing
 * that rejection resumes in a later task, while `execCommand("copy")` is only
 * honoured while the user's gesture is still on the stack. A fallback reached
 * through the rejection therefore arrives too late to write anything. Browsers
 * keep the async API first, where it is the supported path and is not gated on
 * activation this way.
 */
export async function copyText(text: string): Promise<boolean> {
  if (isTauri() && legacyCopy(text)) {
    return true;
  }
  try {
    // Reaching through a missing `navigator.clipboard` throws synchronously
    // here, so one catch covers both "absent" and "rejected".
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return legacyCopy(text);
  }
}

function legacyCopy(text: string): boolean {
  if (typeof document === "undefined") return false;

  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  // `execCommand("copy")` copies the selection, and an element that is
  // `display:none` or detached cannot hold one — so it has to be in the
  // document and merely invisible.
  area.style.position = "fixed";
  area.style.top = "0";
  area.style.left = "0";
  area.style.opacity = "0";
  area.style.pointerEvents = "none";
  // `select()` takes focus in a real engine; hand it back so copying does not
  // knock the user out of whatever they were typing in.
  const previous = document.activeElement;
  document.body.appendChild(area);
  try {
    area.select();
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    area.remove();
    if (previous instanceof HTMLElement) previous.focus();
  }
}
