import { execFileSync } from "node:child_process";
import { dirname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import type { Plugin } from "vite";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const generator = resolve(root, "scripts/protocol-versions.mjs");
const baseline = resolve(root, "protocol/baseline.json");
const changes = resolve(root, "protocol/changes");

export function generateProtocolVersions(): void {
  execFileSync(process.execPath, [generator, "generate"], { stdio: "inherit" });
}

export function protocolGenerationPlugin(): Plugin {
  let cleanup: (() => void) | undefined;
  return {
    name: "protocol-generation",
    configureServer(server) {
      server.watcher.add([baseline, changes, generator]);
      const regenerate = (event: string, path: string) => {
        if (["add", "change", "unlink", "addDir", "unlinkDir"].includes(event)
          && (path === baseline || path === generator || path === changes || path.startsWith(changes + sep))) {
          try {
            generateProtocolVersions();
          } catch (error) {
            server.config.logger.error(`protocol-generation failed: ${String(error)}`);
          }
        }
      };
      server.watcher.on("all", regenerate);
      cleanup = () => { server.watcher.off("all", regenerate); };
    },
    closeBundle() {
      cleanup?.();
    },
  };
}
