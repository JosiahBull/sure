import { type Page } from "@playwright/test";

import { test, expect } from "./fixtures";

// The import UI, which had no test at any tier until the four panels became one. What is worth
// covering here and nowhere else is the *browser* half: that a file picked in a file input reaches
// the endpoint at all, that several files become one upload, that what comes back is rendered as a
// preview with the account it is going to, and that the refusal path offers a way forward.
//
// Deliberately **preview only**. This suite shares one seeded demo database with the visual
// baselines in `app.spec.ts`, so a committed import would move the dashboard's figures and break
// screenshots that have nothing to do with import. A dry run writes nothing, and it exercises
// every part of the wiring except the one flag — `packages/api-tests/specs/import.spec.ts` asserts
// preview-equals-commit for each source, which is the property that makes that split safe.

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

/** An ASB export for `account`, in the shape ASB really writes: preamble, blank line, then rows. */
function asbCsv(account: string, closing: string, rows: string[]): Buffer {
  return Buffer.from(
    "Created date / time : 3 August 2026 / 16:27:53\r\n" +
      `Bank 12; Branch 3136; Account ${account} (Streamline)\r\n` +
      "From date 20260101\r\nTo date 20260803\r\n" +
      `Ledger Balance : ${closing} as of 20260803\r\n\r\n` +
      "Date,Unique Id,Tran Type,Cheque Number,Payee,Memo,Amount\r\n" +
      rows.join("\r\n") +
      "\r\n"
  );
}

const ONE_ROW = ['2026/07/27,2026072701,EFTPOS,,"COUNTDOWN QUEEN ST","EFTPOS",-5.00'];
const TWO_ROWS = [
  ...ONE_ROW,
  '2026/07/28,2026072801,D/C,,"D/C FROM ACME CORP","ACME CORP",100.00',
];

const csvFile = (name: string, buffer: Buffer) => ({ name, mimeType: "text/csv", buffer });

/** The panel's file input is hidden and clicked programmatically, so target it directly. */
const fileInput = (page: Page) => page.locator(".import input[type=file]");

test("the Import page reads a dropped bank export and previews where it is going", async ({
  page,
}) => {
  await goto(page, "/settings/import");
  await expect(page.getByRole("heading", { name: "Import", level: 1 })).toBeVisible();

  await fileInput(page).setInputFiles(
    csvFile("export.csv", asbCsv("0000123-50", "95.00", TWO_ROWS))
  );

  // Recognised from the bytes — nothing here told it what the file was.
  await expect(page.getByText("ASB transaction export")).toBeVisible();
  const row = page.locator(".import tbody tr").first();
  await expect(row).toContainText("12-3136-0000123-50");
  await expect(row).toContainText("Streamline");
  await expect(row).toContainText("export.csv");

  // No account in the demo data answers to that number, so nothing is preselected and the commit
  // is refused until someone says where it goes. This is the state the picker exists for.
  await expect(page.getByRole("button", { name: /^Import 0/ })).toBeDisabled();

  // Choosing one re-runs the preview, because the row count depends on the *target* account's
  // cutover — until an account is chosen the count can only be "all of them", and picking one can
  // only reduce it. The number on the button has to be the number the commit imports.
  // By the option's text, not its exact label: the picker appends the owner to every account, so
  // matching "Everyday" outright breaks on a change that has nothing to do with this assertion.
  const select = row.locator("select");
  await select.selectOption({
    label: await select.locator("option", { hasText: "Everyday" }).first().innerText(),
  });
  await expect(row).toContainText("you chose it");
  const commit = page.getByRole("button", { name: /^Import \d+/ });
  await expect(commit).toBeEnabled();
  await expect(commit).not.toHaveText(/^Import 0/);

  // Nothing is written until that button is pressed, which is what the copy promises.
  await expect(page.getByText("Nothing is saved until you import")).toBeVisible();
});

test("several files picked at once arrive as one upload, described account by account", async ({
  page,
}) => {
  await goto(page, "/settings/import");
  await fileInput(page).setInputFiles([
    csvFile("chequing.csv", asbCsv("0000123-50", "95.00", ONE_ROW)),
    csvFile("savings.csv", asbCsv("0000123-51", "500.00", ONE_ROW)),
  ]);

  // One request carrying both — the browser packs them into a zip, which is the only shape whose
  // cross-file checks can run. Two files in, two accounts described.
  await expect(page.getByText(/ASB transaction export · 2 accounts/)).toBeVisible();
  const rows = page.locator(".import tbody tr");
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toContainText("chequing.csv");
  await expect(rows.nth(1)).toContainText("savings.csv");
});

/**
 * Skipping one file has to reach the server as a decision, and the row has to read as one.
 *
 * The bug: "skip this one" and "nothing matched yet" were the same value, so a skip sent *no*
 * assignment at all — which only silences the top routing tier and leaves the five below it to
 * place the file anyway. The row read as skipped and imported regardless.
 */
test("skipping one file zeroes it and says so on the wire, not just on screen", async ({
  page,
}) => {
  await goto(page, "/settings/import");
  const assigns: string[] = [];
  // Watched rather than stubbed: the preview itself is real, and what is being asserted is the
  // parameter the panel sends when a row is skipped.
  page.on("request", (r) => {
    if (r.url().includes("/api/import?")) assigns.push(new URL(r.url()).search);
  });

  await fileInput(page).setInputFiles([
    csvFile("chequing.csv", asbCsv("0000123-50", "95.00", ONE_ROW)),
    csvFile("savings.csv", asbCsv("0000123-51", "500.00", ONE_ROW)),
  ]);
  const rows = page.locator(".import tbody tr");
  await expect(rows).toHaveCount(2);

  // Matched on the option's text rather than its exact label: the picker names the owner beside
  // the account, and this test is about the skip, not about how a target is spelled.
  const target = rows.nth(0).locator("select");
  await target.selectOption({
    label: await target.locator("option", { hasText: "Everyday" }).first().innerText(),
  });
  await expect(rows.nth(0)).toContainText("you chose it");
  await rows.nth(1).locator("select").selectOption("skip");
  await expect(rows.nth(1)).toContainText("skipped");

  // The one that matters: `:skip` is on the request, so the server's evidence tiers can't place
  // the file behind the reader's back.
  await expect(() => {
    expect(assigns.at(-1)).toContain("12-3136-0000123-51%3Askip");
  }).toPass();
  // …and the button counts one account, not two.
  await expect(page.getByRole("button", { name: /^Import \d+ into 1 account$/ })).toBeEnabled();
});

/**
 * The whole point of the change: one file's conflict is one file's problem. This used to be a 422
 * that took the preview with it — no table, no picker, and the only advice pointing at another
 * screen. Stubbed, because reaching it for real needs an unsynced feed on a seeded account, and
 * this suite shares its database with the visual baselines.
 */
test("a held-up file states its conflict in its own row and leaves the rest importable", async ({
  page,
}) => {
  const item = (sourceAccount: string, accountId: number, blocked: unknown) => ({
    source_account: sourceAccount,
    account_id: accountId,
    account_name: "Everyday",
    matched_by: "assigned",
    sources: ["export.csv"],
    label: "Streamline",
    covered_from: "2026-07-27",
    covered_to: "2026-07-28",
    rows_total: 2,
    would_import: blocked ? 0 : 2,
    imported: 0,
    skipped: 0,
    held_back: 0,
    cutover: null,
    reconciliation: null,
    blocked,
    extras: [],
    warnings: [],
  });
  await page.route("**/api/import?**", (route) =>
    route.fulfill({
      json: {
        dry_run: true,
        source: "asb_csv",
        items: [
          item("12-3136-0000123-50", 1, {
            reason: "unsynced_feed",
            message:
              "Emergency Fund is connected to Akahu — Emergency Fund, which has not posted a " +
              "transaction yet, so this import cannot tell which period belongs to it.",
            feeds: [{ provider_id: 77, name: "Akahu — Emergency Fund" }],
          }),
          item("12-3136-0000123-51", 2, null),
        ],
        warnings: [],
      },
    })
  );

  await goto(page, "/settings/import");
  await fileInput(page).setInputFiles(
    csvFile("export.csv", asbCsv("0000123-50", "95.00", TWO_ROWS))
  );

  const held = page.locator(".import tbody tr").first();
  // The conflict is in the row, not in a page-level banner that killed the table.
  await expect(held.locator(".blocked")).toContainText("has not posted a transaction yet");
  await expect(held).toContainText("held up");
  // All three ways out, in the row: skip it, sync the feed by name, or say what it owns from.
  await expect(held.getByRole("button", { name: "Skip this file" })).toBeEnabled();
  await expect(held.getByRole("button", { name: "Sync Akahu — Emergency Fund" })).toBeEnabled();
  await expect(held.locator("input[type=date]")).toBeVisible();

  // And the rest of the upload is still importable — which is the whole complaint this fixes.
  await expect(page.getByText(/1 file above is held up and won't be imported/)).toBeVisible();
  await expect(page.getByRole("button", { name: /^Import 2 into 1 account$/ })).toBeEnabled();
});

test("a file that is no export at all is refused, and offers the source picker", async ({
  page,
}) => {
  await goto(page, "/settings/import");
  await fileInput(page).setInputFiles({
    name: "holiday.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("not an export at all"),
  });

  await expect(page.locator(".import .error-banner")).toContainText(
    "doesn't look like any export Sure can read"
  );
  // The way out: say what the file is, rather than being told to go away.
  await expect(page.getByText("Not what you expected?")).toBeVisible();
  await expect(page.locator(".import select")).toBeVisible();
});

test("every Import affordance leads to the one import page", async ({ page }) => {
  // The transactions page's button pointed at Settings → Rules for as long as it existed, which is
  // the drift this test exists to stop coming back.
  await goto(page, "/transactions");
  await expect(page.getByRole("link", { name: "Import" })).toHaveAttribute(
    "href",
    "#/settings/import"
  );

  await goto(page, "/settings/accounts");
  await expect(page.getByRole("link", { name: "Import", exact: true }).first()).toHaveAttribute(
    "href",
    "#/settings/import"
  );
});
