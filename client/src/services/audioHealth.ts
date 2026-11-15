import { isTauri } from "./platform";

export type AudioBootHealth = "healthy" | "wedged" | "unknown";

let cached: Promise<boolean> | null = null;

const VERDICT_WAIT_LIMIT_MS = 6000;

/**
 * Resolves true when opening the OS audio device is safe.
 *
 * On Linux, WebKitGTK opens the audio device synchronously on the page main
 * thread inside `new AudioContext()`; when the system audio server is wedged
 * (streams hang while its control plane still answers) that open freezes the
 * entire page. The Tauri shell probes the device from a killable child
 * process (`audio_boot_health`) and this asks for its verdict. Non-Tauri
 * hosts and every failure mode — old shell without the command, probe
 * machinery breakage, verdict timeout — resolve true: fail-open to today's
 * behavior, so this gate can never itself block a boot.
 */
export function audioDeviceSafe(): Promise<boolean> {
  cached ??= probe();
  return cached;
}

async function probe(): Promise<boolean> {
  if (!isTauri()) return true;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    // The shell's probe self-bounds at ~3s; this race only guards against the
    // gate itself hanging the boot it exists to protect.
    const verdict = await Promise.race([
      invoke<AudioBootHealth>("audio_boot_health"),
      new Promise<AudioBootHealth>((resolve) => setTimeout(() => resolve("unknown"), VERDICT_WAIT_LIMIT_MS)),
    ]);
    return verdict !== "wedged";
  } catch {
    // Missing commands are expected while a remote deployment rolls out ahead
    // of a new shell.
    return true;
  }
}
