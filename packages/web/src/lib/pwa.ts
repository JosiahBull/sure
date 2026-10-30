// What an installed app needs from its service worker in place of browser chrome.
//
// The worker precaches the app shell (vite.config.ts), so a launch paints the build that worker
// holds — not whatever is on the server. Two things then have to happen for a deploy to reach
// the screen: the browser has to notice a new worker, and the page has to load again under it.
// vite-plugin-pwa's registration (the injected registerSW.js) does neither on its own: the
// browser looks for a new worker only on a navigation, and nothing reloads the page once one has
// taken over. On an iPhone a navigation is rare: a Home Screen app goes to the background rather
// than closing, and resuming it is not one, so a phone could sit on an old build for days — and,
// when it did finally cold-launch, paint the old one once more while the new worker installed.
//
// So: look for a new worker whenever the app comes back to the foreground, and once one has
// taken over, reload at the next moment that costs nothing — a change of page, which remounts
// everything anyway, or the next return to the foreground. Never under someone's thumb: a reload
// mid-page would drop whatever they had half-typed.
//
// This relies on the worker the plugin generates for `registerType: "autoUpdate"`, which skips
// waiting and claims open pages as soon as it has installed — that is what fires
// `controllerchange` below. The plugin only generates it that way while `injectRegister` is left
// at its default — so the plugin's own registerSW.js registers the worker, and this module
// registers nothing.

import { router } from "./router.svelte";

/** The page part of the route. A `?tx=` or `?range=` change stays on the page, so it is no seam. */
const pageOf = (path: string) => path.split("?")[0];

let updateReady = false;

function markUpdateReady(): void {
  if (updateReady) return;
  updateReady = true;
  console.info("sure: a new version is installed; it loads at the next change of page");
}

// Absent outside a secure context — the app served over plain http on a LAN address.
const sw = navigator.serviceWorker as ServiceWorkerContainer | undefined;

if (sw) {
  // A new worker taking control of a page that already had one is a new build. The first worker
  // to claim a page that had none is just the install, and the page is already that build.
  let controlled = sw.controller !== null;
  sw.addEventListener("controllerchange", () => {
    if (controlled) markUpdateReady();
    controlled = true;
  });

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible") return;
    if (updateReady) {
      window.location.reload();
      return;
    }
    // Offline is the usual reason this rejects, and the next resume will try again.
    sw.getRegistration()
      .then((registration) => registration?.update())
      .catch(() => {});
  });

  let shownPage = pageOf(router.path);
  window.addEventListener("hashchange", () => {
    const page = pageOf(router.path);
    if (updateReady && page !== shownPage) window.location.reload();
    shownPage = page;
  });
}

// The origin's storage is evicted under pressure unless it is marked persistent — the precached
// shell with it, so the next launch would have to fetch it before it could paint. WebKit grants
// this to a Home Screen web app; a browser tab is left to its browser's heuristics, not asked.
if (window.matchMedia("(display-mode: standalone)").matches) {
  navigator.storage?.persist?.().catch(() => {});
}
