import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import wasm from "vite-plugin-wasm";
import topLevelAwait from "vite-plugin-top-level-await";
import { VitePWA } from "vite-plugin-pwa";

export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
    wasm(),
    topLevelAwait(),
    VitePWA({
      registerType: "autoUpdate",
      manifest: false, // Use public/manifest.json
      workbox: {
        runtimeCaching: [
          {
            urlPattern: /^https:\/\/api\.scryfall\.com\//,
            handler: "NetworkFirst",
            options: {
              cacheName: "scryfall-api",
              expiration: { maxEntries: 500, maxAgeSeconds: 86400 },
            },
          },
          {
            urlPattern: /^https:\/\/cards\.scryfall\.io\//,
            handler: "CacheFirst",
            options: {
              cacheName: "scryfall-images",
              expiration: { maxEntries: 2000, maxAgeSeconds: 604800 },
            },
          },
        ],
      },
    }),
  ],
  build: {
    target: "esnext",
  },
});
