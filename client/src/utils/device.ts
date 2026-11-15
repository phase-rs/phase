/**
 * iOS devices and Android phones, whose browsers kill the tab rather than fail an
 * allocation. iOS Safari reloads the page when WebAssembly memory outgrows a
 * ceiling observed between roughly 300 and 800 MB, and the engine with its card
 * database already sits at about 540 MB, so these devices get one engine
 * instance and the lighter rendering defaults.
 */
export function isMemoryConstrainedDevice(): boolean {
  if (typeof navigator === "undefined") return false;
  const isIOS = /iP(hone|od|ad)/.test(navigator.userAgent)
    || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
  return isIOS || (/Android/.test(navigator.userAgent) && /Mobile/.test(navigator.userAgent));
}
