import path from "node:path";
import { defineConfig, loadEnv } from "vite";

export default defineConfig(({ mode }) => {
  const workspaceRoot = path.resolve(__dirname, "../..");
  const fileEnv = loadEnv(mode, workspaceRoot, "");
  const envVar = (name: string): string => process.env[name] ?? fileEnv[name] ?? "";

  return {
    root: __dirname,
    build: {
      outDir: "dist",
      emptyOutDir: true,
      // `buildBackup` imports shared storage helpers. Their unused deck-repair
      // branch dynamically imports the app's WASM engine, which this tiny
      // bootstrap neither builds nor executes.
      rollupOptions: {
        external: ["@wasm/engine"],
      },
    },
    define: {
      __SUPABASE_URL__: JSON.stringify(envVar("SUPABASE_URL")),
      __SHELL_REMOTE_ORIGIN__: JSON.stringify(
        envVar("SHELL_REMOTE_ORIGIN") || "https://phase-rs.dev",
      ),
      __SHELL_PREVIEW_ORIGIN__: JSON.stringify(
        envVar("SHELL_PREVIEW_ORIGIN") || "https://preview.phase-rs.dev",
      ),
    },
  };
});
