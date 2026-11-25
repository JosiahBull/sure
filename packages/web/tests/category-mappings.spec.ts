import { test, expect } from "./fixtures";

test("configure import mappings on phone and desktop", async ({ page }, testInfo) => {
  const created = await page.request.post("/api/categories", { data: { name: "Everyday meals", kind: "expense" } });
  expect(created.ok()).toBeTruthy();
  const category = await created.json() as { id: number };
  await page.goto("/#/settings/categories");
  await page.getByRole("link", { name: "Import mappings", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Import mappings", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Add mapping", exact: true }).click();
  const add = page.getByRole("form", { name: "New import mapping" });
  await add.getByRole("combobox", { name: "Source", exact: true }).selectOption("csv-upload");
  await add.getByLabel("Source group").fill("Food");
  await add.getByLabel("Source category").fill("Prepared meals");
  await add.getByLabel("Your category").selectOption(String(category.id));
  await add.screenshot({ animations: "disabled", path: testInfo.outputPath("mapping-form-phone.png") });
  await add.getByRole("button", { name: "Save mapping" }).click();
  await page.getByLabel("Search mappings").fill("Prepared meals");
  const row = page.locator(".mapping-row").filter({ hasText: "Prepared meals" });
  await expect(row).toHaveCount(1);
  await expect(row).toContainText("Everyday meals");
  expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)).toBeLessThanOrEqual(1);
  await page.screenshot({ animations: "disabled", path: testInfo.outputPath("mapping-phone.png"), fullPage: true });
  await testInfo.attach("Mapping editor on phone", { path: testInfo.outputPath("mapping-phone.png"), contentType: "image/png" });

  await page.setViewportSize({ width: 1100, height: 850 });
  await page.reload();
  await page.getByLabel("Search mappings").fill("Prepared meals");
  await expect(row).toContainText("Everyday meals");
  await page.screenshot({ animations: "disabled", path: testInfo.outputPath("mapping-desktop.png"), fullPage: true });
  await testInfo.attach("Mapping editor on desktop", { path: testInfo.outputPath("mapping-desktop.png"), contentType: "image/png" });
  // Native buttons and selects remain operable by keyboard after saving.
  await row.getByRole("button", { name: "Edit mapping for Prepared meals" }).focus();
  await page.keyboard.press("Enter");
  const edit = page.getByRole("form", { name: "Edit mapping for Prepared meals" });
  await edit.getByLabel("Your category").selectOption("");
  await edit.getByRole("button", { name: "Save mapping" }).click();
  await expect(row).toContainText("Unmapped");
  await expect(row.getByRole("button")).toBeFocused();
  await page.getByLabel("Unmapped only").check();
  await expect(row).toHaveCount(1);
  await row.getByRole("button").click();
  await edit.getByLabel("Your category").selectOption(String(category.id));
  await edit.getByRole("button", { name: "Save mapping" }).click();
  await expect(page.getByText("No mappings match these filters.")).toBeVisible();
  await page.getByLabel("Unmapped only").uncheck();
  await expect(row).toContainText("Everyday meals");
  await page.request.put(`/api/categories/${category.id}`, { data: { name: "Food and drink", kind: "expense" } });
  await page.reload();
  await page.getByLabel("Search mappings").fill("Prepared meals");
  await expect(row).toContainText("Food and drink");
  await page.request.delete(`/api/categories/${category.id}`);
  await page.reload();
  await page.getByLabel("Search mappings").fill("Prepared meals");
  await expect(row).toContainText("Unmapped");
  await page.setViewportSize({ width: 402, height: 874 });
  await page.getByLabel("Search mappings").fill("Universities");
  await expect(page.locator(".mapping-row").first()).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)).toBeLessThanOrEqual(1);
});

test("mapping save errors retain the selection and allow a retry", async ({ page }) => {
  await page.goto("/#/settings/categories?view=mappings");
  await page.getByLabel("Search mappings").fill("Convenience stores");
  await page.getByRole("button", { name: "Edit mapping for Convenience stores" }).click();
  const edit = page.getByRole("form", { name: "Edit mapping for Convenience stores" });
  await edit.getByLabel("Your category").selectOption("");
  await page.route("**/api/category-mappings/*", route => route.fulfill({
    status: 422, contentType: "application/json", body: JSON.stringify({ error: { message: "Destination no longer exists." } }),
  }));
  await edit.getByRole("button", { name: "Save mapping" }).click();
  await expect(page.getByRole("alert")).toContainText("Destination no longer exists.");
  await expect(edit.getByLabel("Your category")).toHaveValue("");
  await expect(edit.getByRole("button", { name: "Save mapping" })).toBeEnabled();
  await page.unroute("**/api/category-mappings/*");
  await edit.getByRole("button", { name: "Save mapping" }).click();
  await expect(page.getByRole("alert")).toHaveCount(0);
  await expect(page.locator(".mapping-row")).toContainText("Unmapped");
});


test("mapping loading and connection errors have a working retry", async ({ page }, testInfo) => {
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  await page.route("**/api/category-mappings", async route => {
    await gate;
    await route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ error: { message: "Import mappings are temporarily unavailable." } }) });
  });
  try {
    await page.goto("/#/settings/categories?view=mappings");
    await expect(page.getByRole("status")).toContainText("Loading import mappings");
    release();
    await expect(page.getByRole("alert")).toContainText("Import mappings are temporarily unavailable.");
    await page.screenshot({ animations: "disabled", path: testInfo.outputPath("mapping-load-error.png"), fullPage: true });
    await page.unroute("**/api/category-mappings");
    await page.getByRole("button", { name: "Try again" }).click();
    await expect(page.getByRole("alert")).toHaveCount(0);
    await expect(page.locator(".mapping-row").first()).toBeVisible();
  } finally { release(); }
});
