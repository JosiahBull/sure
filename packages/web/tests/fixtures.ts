import { test as base, expect } from "@playwright/test";

import { DEMO_NOW } from "./demo-date";

/**
 * The suite's `test`, with the page's clock fixed to the same instant the demo data is
 * seeded against (see demo-date.ts for why both are needed).
 *
 * `setFixedTime` — not `install()` — because only `Date` needs to lie: timers still have
 * to run for the number tweens and CSS transitions to settle, or every screenshot would
 * catch the app mid-animation.
 */
export const test = base.extend({
  page: async ({ page }, use) => {
    await page.clock.setFixedTime(DEMO_NOW);
    await use(page);
  },
});

export { expect };
