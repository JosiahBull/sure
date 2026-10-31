// Performance marks for the moments a launch exists for, read back by the connection page
// (pages/Debug.svelte). The browser already times the document and every request; what it
// cannot know is when the figures reached the screen.

/** The dashboard first drew figures kept from an earlier launch (lastKnown.ts). */
export const KEPT_DRAWN = "sure:kept-drawn";
/** The dashboard first drew the server's figures. */
export const FRESH_DRAWN = "sure:fresh-drawn";

/**
 * Mark `name` in the frame that draws what was just assigned, once per page load: later loads
 * of the same page are a change of period, not the launch.
 */
export function markDrawn(name: string): void {
  if (typeof performance.mark !== "function" || typeof performance.getEntriesByName !== "function") return;
  requestAnimationFrame(() => {
    if (performance.getEntriesByName(name, "mark").length === 0) performance.mark(name);
  });
}
