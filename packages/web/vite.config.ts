import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { VitePWA } from "vite-plugin-pwa";

// The dev server proxies /api to the Rust backend; in production the backend serves
// the built assets directly (WEB_DIR), so the SPA and API share an origin.
export default defineConfig({
  plugins: [
    svelte(),
    VitePWA({
      registerType: "autoUpdate",
      includeAssets: ["favicon.svg"],
      manifest: {
        name: "Sure — Finance",
        short_name: "Sure",
        description: "A fast, local financial tracker.",
        // Cold-launch chrome + app-switcher tint. Aligned with the dark --bg
        // (and the dark-backgrounded app icon) so the launch reads as one piece;
        // once the SPA mounts, src/lib/theme.svelte.ts drives the live
        // <meta name="theme-color"> to match the active light/dark palette.
        theme_color: "#0b1120",
        background_color: "#0b1120",
        display: "standalone",
        start_url: "/",
        icons: [
          { src: "icon-192.png", sizes: "192x192", type: "image/png" },
          { src: "icon-512.png", sizes: "512x512", type: "image/png" },
          { src: "icon-512.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
        ],
      },
      workbox: {
        // Never cache API calls; always hit the network for fresh financial data.
        navigateFallbackDenylist: [/^\/api/],
        runtimeCaching: [
          {
            urlPattern: /^\/api\//,
            handler: "NetworkOnly",
          },
        ],
      },
    }),
  ],
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8080",
        changeOrigin: true,
      },
    },
  },
  build: {
    target: "es2022",
    // Keep chunks lean; report anything unexpectedly large.
    chunkSizeWarningLimit: 600,
  },
});
