import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "happy-dom",
    include: ["src/**/*.test.{ts,tsx}"],
    exclude: ["src/**/*.integration.test.{ts,tsx}"],
    setupFiles: ["src/test-setup.ts"],
    pool: "threads",
    poolOptions: {
      threads: {
        singleThread: false,
      },
    },
    coverage: {
      provider: "v8",
      reporter: ["text", "lcov"],
      include: ["src/**/*.{ts,tsx}"],
      exclude: ["src/**/__tests__/**", "src/**/*.test.*", "src/wasm/**"],
      thresholds: {
        lines: 10,
        functions: 10,
      },
    },
  },
});
