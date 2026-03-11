// Sidecar lifecycle management for Tauri desktop builds.
//
// To set up the sidecar binary for development:
// cargo build -p phase-server
// cp target/debug/phase-server client/src-tauri/binaries/phase-server-$(rustc --print host-tuple)

import { Command } from "@tauri-apps/plugin-shell";

/** Check whether we are running inside a Tauri webview. */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export interface SidecarHandle {
  port: number;
  kill: () => Promise<void>;
}

/** Module-level handle for cleanup on page unload. */
let activeSidecar: SidecarHandle | null = null;

/**
 * Spawn the phase-server sidecar binary on an available port.
 * Scans ports 8080-8089 and performs a health check before returning.
 */
export async function spawnSidecar(port = 8080): Promise<SidecarHandle> {
  if (!isTauri()) {
    throw new Error("Sidecar is only available in Tauri desktop builds");
  }

  const maxPort = port + 10;

  for (let tryPort = port; tryPort < maxPort; tryPort++) {
    // Check if port is already in use by trying a health check
    const alreadyRunning = await checkHealth(tryPort);
    if (alreadyRunning) {
      // Server already running on this port -- reuse it
      const handle: SidecarHandle = {
        port: tryPort,
        kill: async () => {
          // Not our process to kill
        },
      };
      activeSidecar = handle;
      return handle;
    }

    try {
      const handle = await trySpawnOnPort(tryPort);
      activeSidecar = handle;
      return handle;
    } catch {
      // Port may be in use by something else, try next
      continue;
    }
  }

  throw new Error(`Failed to spawn sidecar on ports ${port}-${maxPort - 1}`);
}

async function trySpawnOnPort(port: number): Promise<SidecarHandle> {
  const command = Command.sidecar("binaries/phase-server", [], {
    env: {
      PORT: String(port),
    },
  });

  const child = await command.spawn();

  // Health check: poll /health every 500ms, up to 10 attempts (5s)
  const maxAttempts = 10;
  for (let i = 0; i < maxAttempts; i++) {
    await sleep(500);
    const healthy = await checkHealth(port);
    if (healthy) {
      return {
        port,
        kill: () => child.kill(),
      };
    }
  }

  // Timed out -- kill the process and throw
  await child.kill();
  throw new Error(`Sidecar health check timed out on port ${port}`);
}

async function checkHealth(port: number): Promise<boolean> {
  try {
    const response = await fetch(`http://localhost:${port}/health`);
    return response.ok;
  } catch {
    return false;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Stop a running sidecar. */
export async function stopSidecar(handle: SidecarHandle): Promise<void> {
  await handle.kill();
  if (activeSidecar === handle) {
    activeSidecar = null;
  }
}

/** Get the currently active sidecar handle, if any. */
export function getActiveSidecar(): SidecarHandle | null {
  return activeSidecar;
}
