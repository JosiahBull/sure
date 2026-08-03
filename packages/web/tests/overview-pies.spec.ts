import { type Page } from "@playwright/test";

import { test, expect } from "./fixtures";

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

  // The deep-link carries the pie's own side of the ledger as `type` alongside the
  // category and the overview's range — an uncategorised slice has only `type` to tell
  // income from outgoings, so it's always sent.
  await expect(page).toHaveURL(/#\/transactions\?category=\d+&type=expense&range=last_12m/);
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
// Nodes are full-width groups; drive them directly like the pie segments. Target them by
// `data-node-id` rather than by text: the chart now draws a category at every level of the
// tree, so `hasText: "Housing"` would also match "Housing" the label of its own children's
// column neighbours, and Playwright's strict mode rejects an ambiguous locator.

/** `in:<id>` / `out:<id>` for a seeded category, resolved by name. */
async function nodeId(page: Page, name: string, side: "in" | "out"): Promise<string> {
  const res = await page.request.get("/api/categories");
  const cats = (await res.json()) as Array<{ id: number; name: string }>;
  const cat = cats.find((c) => c.name === name);
  expect(cat, `category ${name} is seeded`).toBeTruthy();
  return `${side}:${cat!.id}`;
}
const node = (page: Page, id: string) =>
  page.locator(".card", { hasText: "Money flow" }).locator(`g.node[data-node-id="${id}"]`);

test("hovering a sankey node names it and dims the unconnected flows", async ({ page }) => {
  await goto(page, "/");
  const flow = page.locator(".card", { hasText: "Money flow" });
  await node(page, await nodeId(page, "Housing", "out")).dispatchEvent("pointerenter");

  await expect(flow.locator(".tip")).toContainText("Housing"); // tooltip names the node
  // An income root isn't connected to an expense root, so it dims.
  await expect(node(page, await nodeId(page, "Income", "in"))).toHaveAttribute("opacity", "0.25");
});

test("hovering a sankey link shows the flow and its value", async ({ page }) => {
  await goto(page, "/");
  const flow = page.locator(".card", { hasText: "Money flow" });
  await flow.locator("path.link").first().dispatchEvent("pointerenter");
  await expect(flow.locator(".tip")).toContainText("→"); // "source → target"
});

test("clicking a sankey category node opens its filtered transactions", async ({ page }) => {
  await goto(page, "/");
  await node(page, await nodeId(page, "Housing", "out")).dispatchEvent("click");

  // Same deep-link shape as the pie: the node's kind rides along as `type`.
  await expect(page).toHaveURL(/#\/transactions\?category=\d+&type=expense&range=last_12m/);
  await expect(page.locator(".tx-row").first()).toBeVisible();
});

test("the money flow expands into a full-window view", async ({ page }) => {
  await goto(page, "/");
  const flow = page.locator(".card", { hasText: "Money flow" });
  // dispatchEvent, not click(): the app's sticky top toolbar covers whatever Playwright
  // scrolls to the top of the viewport, same reason the pie tests above drive elements
  // directly.
  await flow.getByRole("button", { name: "Expand" }).dispatchEvent("click");

  const dialog = page.getByRole("dialog", { name: "Money flow" });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator("g.node").first()).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});

// The suite's viewport is a phone, where seven columns of two-line labels would be a pile
// rather than a chart — so the money flow shows only as many category levels as it can
// render legibly, and the deep hierarchy has to be exercised at a width that fits it.
test.describe("money flow at desktop width", () => {
  test.use({ viewport: { width: 1440, height: 1000 } });

  test("draws every category level, and a leaf below the top ones", async ({ page }) => {
    await goto(page, "/");
    const flow = page.locator(".card", { hasText: "Money flow" });
    // Power is three deep (Housing > Utilities > Power) — the level the phone width drops.
    await expect(flow.locator(`g.node[data-node-id="${await nodeId(page, "Power", "out")}"]`)).toBeVisible();
    await expect(flow.locator(`g.node[data-node-id="${await nodeId(page, "Salary", "in")}"]`)).toBeVisible();
  });

  test("a node's tooltip gives its share of the flow it came out of", async ({ page }) => {
    await goto(page, "/");
    const flow = page.locator(".card", { hasText: "Money flow" });
    const id = await nodeId(page, "Utilities", "out");
    await flow.locator(`g.node[data-node-id="${id}"]`).dispatchEvent("pointerenter");
    // Utilities is one level down, so the share is of Housing, not of all spending.
    await expect(flow.locator(".tip")).toContainText("%");
  });

  test("a nested node deep-links to its own level, not its root", async ({ page }) => {
    await goto(page, "/");
    const res = await page.request.get("/api/categories");
    const cats = (await res.json()) as Array<{ id: number; name: string }>;
    const utilities = cats.find((c) => c.name === "Utilities")!;
    await page
      .locator(".card", { hasText: "Money flow" })
      .locator(`g.node[data-node-id="out:${utilities.id}"]`)
      .dispatchEvent("click");

    // Utilities, not its parent Housing — an intermediate node used to be unreachable.
    await expect(page).toHaveURL(new RegExp(`#/transactions\\?category=${utilities.id}&type=expense`));
    await expect(page.locator(".tx-row").first()).toBeVisible();
  });

  test("a long tail of hairline categories folds into one Other node", async ({ page }) => {
    // What an all-time window looks like: every category that ever saw a dollar earns a
    // slot, and the ones worth cents render as 1px slivers that crowd out the rest.
    const accounts = await (await page.request.get("/api/accounts")).json();
    const acc = accounts.find((a: { kind: string }) => a.kind === "bank");
    for (let i = 0; i < 20; i++) {
      const cat = await (
        await page.request.post("/api/categories", {
          data: { name: `Tail ${i}`, kind: "expense", parent_id: null, sort_order: 0 },
        })
      ).json();
      await page.request.post("/api/transactions", {
        data: {
          account_id: acc.id,
          posted_at: "2026-03-10",
          amount_minor: -(200 + i * 90),
          currency_code: acc.currency_code,
          description: `tail ${i}`,
          category_id: cat.id,
        },
      });
    }
    await goto(page, "/");
    const flow = page.locator(".card", { hasText: "Money flow" });

    const other = flow.locator('g.node[data-node-id^="other:"]');
    await expect(other).toHaveCount(1);
    await expect(other).toContainText("Other (");
    // Every tail is gone from the diagram, not merely unlabelled.
    for (const id of [0, 7, 19]) {
      await expect(flow.locator("g.node", { hasText: `Tail ${id} ` })).toHaveCount(0);
    }
    // It stands for several categories at once, so there's nothing for a click to open.
    await expect(other).not.toHaveAttribute("role", "button");
    // The categories worth reading keep their own nodes.
    await expect(flow.locator(`g.node[data-node-id="${await nodeId(page, "Housing", "out")}"]`)).toBeVisible();
  });

  test("no two labels in a column overlap, however crowded it gets", async ({ page }) => {
    await goto(page, "/");
    const flow = page.locator(".card", { hasText: "Money flow" });
    const shown = await flow.locator("g.node").evaluateAll((els) =>
      els
        .filter((e) => !e.querySelector("text")?.classList.contains("hidden"))
        .map((e) => {
          const b = (e.querySelector("path") as SVGGraphicsElement).getBBox();
          return { x: Math.round(b.x), y: b.y + b.height / 2 };
        }),
    );
    const byColumn = new Map<number, number[]>();
    for (const s of shown) byColumn.set(s.x, [...(byColumn.get(s.x) ?? []), s.y]);
    for (const [x, ys] of byColumn) {
      const sorted = [...ys].sort((a, b) => a - b);
      for (let i = 1; i < sorted.length; i++) {
        // Two label lines need ~26px; anything tighter is a legible-looking collision.
        expect(sorted[i] - sorted[i - 1], `labels overlap in column x=${x}`).toBeGreaterThanOrEqual(26);
      }
    }
  });

  test("a childless top-level category sits beside the hub, not out with the leaves", async ({ page }) => {
    await goto(page, "/");
    const flow = page.locator(".card", { hasText: "Money flow" });
    const box = async (id: string) => (await flow.locator(`g.node[data-node-id="${id}"] path`).first().boundingBox())!;
    // Transport has no children, so d3's default alignment would push it to the far column
    // alongside other branches' grandchildren. It belongs in the first expense column with
    // the other roots.
    const transport = await box(await nodeId(page, "Transport", "out"));
    const housing = await box(await nodeId(page, "Housing", "out"));
    const power = await box(await nodeId(page, "Power", "out"));
    expect(Math.abs(transport.x - housing.x)).toBeLessThan(2);
    expect(power.x).toBeGreaterThan(transport.x + 100);
  });
});
