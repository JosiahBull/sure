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
        // The dark --bg in src/app.css, so an Android launch splash hands over to the first
        // paint without a change of colour; once the SPA mounts, src/lib/theme.svelte.ts drives
        // the live <meta name="theme-color"> for whichever palette is showing. iOS reads
        // neither: its launch screen and status bar come from index.html and the page itself.
        // Not the icon's navy tile (#0b1120), which is the previous palette's --bg.
        theme_color: "#0b0b0b",
        background_color: "#0b0b0b",
        display: "standalone",
        // The app's identity, pinned so that a future change to start_url does not make every
        // install a different app. Safari has read it since 16.4.
        id: "/",
        start_url: "/",
        icons: [
          { src: "icon-192.png", sizes: "192x192", type: "image/png" },
          { src: "icon-512.png", sizes: "512x512", type: "image/png" },
          // Its own file: the mark shrunk into the safe circle on a square tile. All four PNGs
          // (and index.html's apple-touch-icon) come from scripts/generate-icons.mjs.
          { src: "icon-maskable-512.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
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
