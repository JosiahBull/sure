import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { VitePWA } from "vite-plugin-pwa";

/**
 * A hash of the generated API types, which `src/lib/lastKnown.ts` stamps its store with: a
 * change to any response shape discards what an older build kept, rather than drawing it with
 * code that expects something else. The types rather than `openapi.json` because they are what
 * this code is compiled against.
 */
function apiSchemaHash(): string {
  const types = readFileSync(new URL("../client/src/schema.d.ts", import.meta.url));
  return createHash("sha256").update(types).digest("hex").slice(0, 12);
}

/** The release version, from the root manifest that `scripts/check-versions.sh` holds to Cargo's. */
function appVersion(): string {
  const manifest = readFileSync(new URL("../../package.json", import.meta.url), "utf8");
  return (JSON.parse(manifest) as { version: string }).version;
}

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
        // The fonts join the shell in the precache. Left to the HTTP cache (a day, then a week
        // of stale-while-revalidate) they were the one part of a launch that could still wait
        // on the network, and a late swap reflows every figure on the page.
        globPatterns: ["**/*.{js,css,html,woff2}"],
        navigateFallbackDenylist: [/^\/api/],
        // No runtime route for /api, deliberately: an unrouted request never enters a workbox
        // handler, so the browser fetches it exactly as it would without a service worker and
        // nothing here can ever answer one from a cache. Reads that should paint before the
        // network answers are kept by the app instead (src/lib/lastKnown.ts), where a mutation
        // can forget them. (The `NetworkOnly` route that used to sit here never matched: a
        // RegExp route is tested against the full URL, so `^/api/` could not match
        // `https://host/api/...` — it documented the intent without enforcing anything.)
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
  define: {
    __API_SCHEMA_HASH__: JSON.stringify(apiSchemaHash()),
    __APP_VERSION__: JSON.stringify(appVersion()),
  },
  build: {
    target: "es2022",
    // Keep chunks lean; report anything unexpectedly large.
    chunkSizeWarningLimit: 600,
  },
});
