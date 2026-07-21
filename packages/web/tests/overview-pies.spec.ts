import { test, expect, type Page } from "@playwright/test";

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

// The donut segments are full circles (only their arc is painted), so a normal click/hover
// targets the empty centre. Drive them directly and target by aria-label.

test("hovering a pie segment names it and greys the others", async ({ page }) => {
  await goto(page, "/");
  const pie = page.locator(".card", { hasText: "Where money went" });
  await pie.locator('svg .seg[aria-label="Housing"]').dispatchEvent("pointerenter");

  await expect(pie.locator(".pie-center .cl")).toHaveText("Housing"); // centre names the segment
  // Whole dollars, no cents — cents made the value overflow the donut hole.
  await expect(pie.locator(".pie-center .cv")).not.toContainText(".");
  await expect(pie.locator("svg .seg.dim")).not.toHaveCount(0); // the others are dimmed
  await expect(pie.locator("svg .seg:not(.dim)")).toHaveCount(1); // only the hovered one is lit
});

test("the income pie is interactive too", async ({ page }) => {
  await goto(page, "/");
  const pie = page.locator(".card", { hasText: "Where money came from" });
  await pie.locator("svg .seg").first().dispatchEvent("pointerenter");
  await expect(pie.locator(".pie-center .cl")).not.toHaveText("total");
});

test("clicking a pie segment opens transactions filtered to that category and range", async ({ page }) => {
  await goto(page, "/");
  const pie = page.locator(".card", { hasText: "Where money went" });
  await pie.locator('svg .seg[aria-label="Housing"]').dispatchEvent("click");

  await expect(page).toHaveURL(/#\/transactions\?category=\d+&range=last_12m/);
  await expect(page.locator(".tx-row").first()).toBeVisible();
});

test("a category deep-link includes the whole subtree", async ({ page }) => {
  const res = await page.request.get("/api/categories");
  const cats = (await res.json()) as Array<{ id: number; name: string }>;
  const food = cats.find((c) => c.name === "Food");
  expect(food).toBeTruthy();

  await goto(page, `/transactions?category=${food!.id}&range=all`);
  // Food's spend is filed under its children (Groceries / Dining out); an exact-match
  // filter would show nothing, so visible rows prove the subtree filter works.
  await expect(page.locator(".tx-row").first()).toBeVisible();
});

test("the legend mirrors the pie and is clickable", async ({ page }) => {
  await goto(page, "/");
  const pie = page.locator(".card", { hasText: "Where money went" });
  await pie.locator(".legend-row", { hasText: "Housing" }).hover();
  await expect(pie.locator("svg .seg.dim")).not.toHaveCount(0); // hovering the legend greys the pie

  await pie.locator(".legend-row", { hasText: "Housing" }).click();
  await expect(page).toHaveURL(/#\/transactions\?category=\d+/);
});

// ---- Sankey (Money flow) -------------------------------------------------------------
// Nodes are full-width groups; drive them directly like the pie segments.

test("hovering a sankey node names it and dims the unconnected flows", async ({ page }) => {
  await goto(page, "/");
  const flow = page.locator(".card", { hasText: "Money flow" });
  await flow.locator("g.node", { hasText: "Housing" }).dispatchEvent("pointerenter");

  await expect(flow.locator(".tip")).toContainText("Housing"); // tooltip names the node
  // An income node isn't connected to an expense node, so it dims.
  await expect(flow.locator("g.node", { hasText: "Income" })).toHaveAttribute("opacity", "0.25");
});

test("hovering a sankey link shows the flow and its value", async ({ page }) => {
  await goto(page, "/");
  const flow = page.locator(".card", { hasText: "Money flow" });
  await flow.locator("path.link").first().dispatchEvent("pointerenter");
  await expect(flow.locator(".tip")).toContainText("→"); // "source → target"
});

test("clicking a sankey category node opens its filtered transactions", async ({ page }) => {
  await goto(page, "/");
  const flow = page.locator(".card", { hasText: "Money flow" });
  await flow.locator("g.node", { hasText: "Housing" }).dispatchEvent("click");

  await expect(page).toHaveURL(/#\/transactions\?category=\d+&range=last_12m/);
  await expect(page.locator(".tx-row").first()).toBeVisible();
});
