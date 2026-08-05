import { test, expect } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount, makeZip, postOversized } from "../helpers";

// Akahu serves an account about two years of history; ASB's own CSV export reaches seven, so
// this upload is how an account's ledger gets extended past the feed. These specs cover what
// the upload path can get wrong on its own: reading the file, deriving the cutover from the
// account's other feeds, previewing without writing, deduping a re-upload, undoing, and
// refusing a wrong account kind. The text repair, per-type field mapping, and malformed-file
// rejection rules are unit tested in `sure_providers::asb`.

type AsbRow = {
  date: string;
  id: string;
  type: string;
  payee: string;
  memo: string;
  amount: string;
};

/**
 * A well-formed ASB export: the preamble ASB writes above the header (account, declared
 * window, balances), the header row, the blank line ASB leaves, then the transactions.
 * CRLF-terminated, as ASB writes it.
 */
function exportFile(
  rows: AsbRow[],
  opts: { account?: string; ledger?: string } = {}
): ArrayBuffer {
  const account = opts.account ?? "0000123-50";
  const ledger = opts.ledger ?? "100.00";
  const lines = [
    "Created date / time : 03 August 2026 / 16:27:53",
    `Bank 12; Branch 3136; Account ${account} (Streamline)`,
    "From date 20200101",
    "To date 20260803",
    `Avail Bal : ${ledger} as of 20260803`,
    `Ledger Balance : ${ledger} as of 20260803`,
    "Date,Unique Id,Tran Type,Cheque Number,Payee,Memo,Amount",
    "",
    ...rows.map(
      (r) => `${r.date},${r.id},${r.type},,"${r.payee}","${r.memo}",${r.amount}`
    ),
  ];
  return new TextEncoder().encode(lines.join("\r\n") + "\r\n").buffer as ArrayBuffer;
}

const row = (
  date: string,
  id: string,
  type: string,
  payee: string,
  memo: string,
  amount: string
): AsbRow => ({ date, id, type, payee, memo, amount });

/** Three ordinary rows spanning 2020–2022. */
const HISTORY: AsbRow[] = [
  row("2020/01/20", "2020012001", "EFTPOS", "NEW WORLD METRO QUEEN ST AUCKLAND", "EFTPOS", "-12.50"),
  row("2021/06/30", "2021063001", "TFR OUT", "MB TRANSFER", "TO 12-3136- 0000123-51 savings", "-200.00"),
  row("2022/03/15", "2022031501", "D/C", "D/C FROM ACME CORP CONSULTING", "ACME CORP CO IPAYROLL 431", "1500.00"),
];

/**
 * `openingBalance` defaults to **false** here, which is *not* the endpoint's default — it's
 * on. Most specs opt out so their row counts assert the thing they're about rather than
 * absorbing the extra opening row; the default-on behaviour has its own tests below.
 */
async function upload(
  baseURL: string,
  accountId: number,
  body: ArrayBuffer,
  dryRun = false,
  openingBalance = false
) {
  const q = new URLSearchParams({
    dry_run: String(dryRun),
    opening_balance: String(openingBalance),
  });
  return fetch(`${baseURL}/api/accounts/${accountId}/asb/import?${q}`, {
    method: "POST",
    headers: { "Content-Type": "text/csv" },
    body,
  });
}

/** A live feed on the account, so the importer has a cutover to derive. */
async function feed(api: SureClient, accountId: number, payload: string) {
  const { data, response } = await api.POST("/api/providers", {
    body: { name: "Bank feed", kind: "csv", account_id: accountId, enabled: true },
  });
  expect(response.status).toBe(201);
  const sync = await api.POST("/api/providers/{id}/sync", {
    params: { path: { id: data!.id } },
    body: { payload },
  });
  expect(sync.data?.status).toBe("ok");
}

test("imports an ASB export, mapping each transaction type's fields", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });

  const res = await upload(server.baseURL, acc.id, exportFile(HISTORY));
  expect(res.status).toBe(200);
  const r = await res.json();
  expect(r.dry_run).toBe(false);
  expect(r.imported).toBe(3);
  expect(r.skipped).toBe(0);
  expect(r.rows_total).toBe(3);
  expect(r.held_back).toBe(0);
  expect(r.cutover).toBe(null);
  // Echoed back so a wrong upload is obvious.
  expect(r.asb_account).toBe("12-3136-0000123-50");
  expect(r.product).toBe("Streamline");
  expect(r.covered_from).toBe("2020-01-20");
  expect(r.covered_to).toBe("2022-03-15");

  const { data: txns } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  const byAmount = new Map(txns!.map((t) => [t.amount_minor, t]));

  // EFTPOS: the merchant is in the payee, and the memo only restates the type.
  const eftpos = byAmount.get(-1250)!;
  expect(eftpos.description).toBe("NEW WORLD METRO QUEEN ST AUCKLAND");
  expect(eftpos.merchant).toBe("NEW WORLD METRO QUEEN ST AUCKLAND");
  expect(eftpos.posted_at).toBe("2020-01-20T12:00:00+00:00");
  expect(eftpos.provider).toBe(`asb#${acc.id}`);
  expect(eftpos.external_id).toBe("asb:12-3136-0000123-50:2020012001");

  // TFR OUT: the split account number is repaired, and it counts as neither spend nor income.
  const transfer = byAmount.get(-20_000)!;
  expect(transfer.description).toBe("MB TRANSFER TO 12-3136-0000123-51 savings");
  expect(transfer.merchant).toBe(null);

  // D/C: the payer comes from the payee, and the Particulars/Code pair keeps its space.
  const credit = byAmount.get(150_000)!;
  expect(credit.merchant).toBe("ACME CORP CONSULTING");
  expect(credit.description).toContain("ACME CORP CO IPAYROLL 431");
});

test("a transfer row is categorised as a transfer, so reports exclude it", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  await upload(server.baseURL, acc.id, exportFile(HISTORY));

  const { data: txns } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  const transfer = txns!.find((t) => t.amount_minor === -20_000)!;
  expect(transfer.category_id).not.toBe(null);

  const { data: cats } = await api.GET("/api/categories", {});
  expect(cats!.find((c) => c.id === transfer.category_id)?.kind).toBe("transfer");

  // The card and salary rows carry no hint, so they stay for the user's own rules.
  expect(txns!.find((t) => t.amount_minor === -1250)?.category_id).toBe(null);
  expect(txns!.find((t) => t.amount_minor === 150_000)?.category_id).toBe(null);
});

test("a dry run reports what a commit would do and writes nothing", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });

  const previewRes = await upload(server.baseURL, acc.id, exportFile(HISTORY), true);
  expect(previewRes.status).toBe(200);
  const preview = await previewRes.json();
  expect(preview.dry_run).toBe(true);
  expect(preview.would_import).toBe(3);
  // Nothing was written, so nothing is reported as written.
  expect(preview.imported).toBe(0);
  expect(preview.skipped).toBe(0);

  const before = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  expect(before.data?.length).toBe(0);

  // The preview's promise is that the commit matches it.
  const commit = await (await upload(server.baseURL, acc.id, exportFile(HISTORY))).json();
  expect(commit.imported).toBe(preview.would_import);
  expect(commit.rows_total).toBe(preview.rows_total);
  expect(commit.held_back).toBe(preview.held_back);
  expect(commit.cutover).toBe(preview.cutover);
  expect(commit.asb_account).toBe(preview.asb_account);
});

test("re-uploading the same export imports nothing new", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });

  const first = await (await upload(server.baseURL, acc.id, exportFile(HISTORY))).json();
  expect(first.imported).toBe(3);

  const second = await (await upload(server.baseURL, acc.id, exportFile(HISTORY))).json();
  expect(second.imported).toBe(0);
  expect(second.skipped).toBe(3);

  const { data: txns } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  expect(txns?.length).toBe(3);
});

test("rows a live feed already covers are held back, not counted twice", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  // The feed owns 2022-01-01 onward.
  await feed(api, acc.id, "date,amount,description,external_id\n2022-01-01,-5.00,Feed row,f1\n");

  const r = await (await upload(server.baseURL, acc.id, exportFile(HISTORY))).json();
  expect(r.cutover).toBe("2022-01-01");
  // The 2022-03-15 row falls inside the feed's window.
  expect(r.held_back).toBe(1);
  expect(r.imported).toBe(2);
  // The whole file is still described, so a preview can show what was left out.
  expect(r.rows_total).toBe(3);

  const { data: txns } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  expect(txns?.filter((t) => t.provider === `asb#${acc.id}`).length).toBe(2);
  expect(txns?.some((t) => t.amount_minor === 150_000)).toBe(false);
  expect(r.warnings.some((w: string) => w.includes("held back"))).toBe(true);
});

/**
 * The exclusion that makes a second upload safe. Without it the importer would see its own
 * 2020 rows as a feed, take those as the cutover, and hold back everything.
 */
test("a second upload derives the same cutover as the first", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  await feed(api, acc.id, "date,amount,description,external_id\n2022-01-01,-5.00,Feed row,f1\n");

  const first = await (await upload(server.baseURL, acc.id, exportFile(HISTORY))).json();
  const second = await (await upload(server.baseURL, acc.id, exportFile(HISTORY))).json();

  expect(second.cutover).toBe(first.cutover);
  expect(second.held_back).toBe(first.held_back);
  expect(second.imported).toBe(0);
  expect(second.skipped).toBe(2);
});

test("a hand-entered row does not set the cutover", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const manual = await api.POST("/api/transactions", {
    body: {
      account_id: acc.id,
      posted_at: "2021-01-01",
      amount_minor: -999,
      description: "Entered by hand",
      is_one_off: false,
    },
  });
  expect(manual.response.status).toBe(201);

  const r = await (await upload(server.baseURL, acc.id, exportFile(HISTORY))).json();
  expect(r.cutover).toBe(null);
  expect(r.imported).toBe(3);
});

/**
 * A linked feed that has never posted is the one state where "no other feed owns anything
 * here" and "a feed owns a period it hasn't written yet" look identical from the ledger.
 * Importing into it would double every row the feed later posts, because dedupe is
 * `(provider, external_id)` and cannot see across `asb#N` and `csv#M`.
 */
test("an unsynced feed refuses the import rather than importing over its window", async ({
  api,
  server,
}) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  // Linked and enabled, but deliberately never synced — no rows, so no derivable cutover.
  const link = await api.POST("/api/providers", {
    body: { name: "Unsynced bank feed", kind: "csv", account_id: acc.id, enabled: true },
  });
  expect(link.response.status).toBe(201);

  const res = await upload(server.baseURL, acc.id, exportFile(HISTORY));
  expect(res.status).toBe(422);
  const message = (await res.json()).error.message;
  // Names the offending feed, so the reader knows which one to sync or disable.
  expect(message).toContain("Unsynced bank feed");
  expect(message).toContain("has not posted a transaction yet");

  // Refused before anything was written.
  const { data: txns } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  expect(txns?.some((t) => t.provider === `asb#${acc.id}`)).toBe(false);
});

test("a disabled feed that never posted does not block the import", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const link = await api.POST("/api/providers", {
    body: { name: "Retired bank feed", kind: "csv", account_id: acc.id, enabled: false },
  });
  expect(link.response.status).toBe(201);

  // Nothing will ever post from it, so there is no period to hold back.
  const r = await (await upload(server.baseURL, acc.id, exportFile(HISTORY))).json();
  expect(r.cutover).toBe(null);
  expect(r.imported).toBe(3);
});

test("the export's closing balance is reconciled against the account's own", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  // The account says it holds $250.00; the export closes at $100.00.
  const val = await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: acc.id } },
    body: { as_of: "2026-08-03", value_minor: 25_000 },
  });
  expect(val.response.status).toBe(201);

  const r = await (await upload(server.baseURL, acc.id, exportFile(HISTORY), true)).json();
  expect(r.ledger_balance_minor).toBe(10_000);
  expect(r.account_balance_minor).toBe(25_000);
  // A mismatch is the strongest available hint the export is for a different account.
  expect(r.warnings.some((w: string) => w.includes("100.00") && w.includes("250.00"))).toBe(true);
  // 100.00 closing − (−12.50 − 200.00 + 1500.00) of movement.
  expect(r.implied_opening_minor).toBe(10_000 - (-1250 - 20_000 + 150_000));
});

/**
 * The same reconciliation, on an account with no valuation at all. Every kind this route
 * accepts accumulates from its own transaction stream, so if the comparison read valuations
 * alone it would never run for any of them — this is the case that proves it doesn't.
 */
test("the closing balance is reconciled against a transaction-derived balance too", async ({
  api,
  server,
}) => {
  // An opening balance is a transaction, not a valuation: the account derives $500.00.
  const acc = await createAccount(api, "Chequing", "bank", "NZD", {
    institution: "ASB",
    opening_balance_minor: 50_000,
    opening_balance_date: "2019-12-31",
  });
  const { data: before } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  expect(before?.length).toBe(1);

  const r = await (await upload(server.baseURL, acc.id, exportFile(HISTORY), true)).json();
  expect(r.ledger_balance_minor).toBe(10_000);
  expect(r.account_balance_minor).toBe(50_000);
  expect(r.warnings.some((w: string) => w.includes("100.00") && w.includes("500.00"))).toBe(true);
});

/**
 * The other half of the zero-suppression: an account whose ledger is empty derives 0, which
 * is the absence of a balance rather than a balance of zero. Warning on it would fire on
 * every first import and train the reader to ignore the one that matters.
 */
test("an account with no balance of its own is not reconciled against", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });

  const r = await (await upload(server.baseURL, acc.id, exportFile(HISTORY), true)).json();
  expect(r.ledger_balance_minor).toBe(10_000);
  expect(r.account_balance_minor).toBe(null);
  expect(r.warnings.some((w: string) => w.includes("but the account"))).toBe(false);
});

test("an account kind with no bank statement is refused", async ({ api, server }) => {
  const acc = await createAccount(api, "Family home", "real_estate");
  const res = await upload(server.baseURL, acc.id, exportFile(HISTORY));
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("can only be imported into");
});

test("an unknown account is a 404", async ({ server }) => {
  const res = await upload(server.baseURL, 987_654, exportFile(HISTORY));
  expect(res.status).toBe(404);
});

test("a file that isn't an ASB export is refused, and the server survives", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  for (const body of [
    new TextEncoder().encode("").buffer as ArrayBuffer,
    new TextEncoder().encode("<html><body>not a csv</body></html>").buffer as ArrayBuffer,
    new TextEncoder().encode("date,amount\n2020-01-01,-5.00\n").buffer as ArrayBuffer,
  ]) {
    const res = await upload(server.baseURL, acc.id, body);
    expect(res.status).toBe(422);
    expect((await res.json()).error.message).toContain("could not read export");
  }
  // Still serving.
  const health = await api.GET("/api/health", {});
  expect(health.response.status).toBe(200);
});

test("undo removes this importer's rows and leaves every other source alone", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  await feed(api, acc.id, "date,amount,description,external_id\n2024-01-01,-5.00,Feed row,f1\n");
  const imported = await (await upload(server.baseURL, acc.id, exportFile(HISTORY))).json();
  expect(imported.imported).toBe(3);

  const undo = await api.DELETE("/api/accounts/{id}/asb/import", {
    params: { path: { id: acc.id } },
  });
  expect(undo.response.status).toBe(200);
  expect(undo.data?.deleted).toBe(3);

  const { data: left } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  expect(left?.length).toBe(1);
  expect(left?.[0].description).toBe("Feed row");

  // Idempotent, and the import can be redone.
  const again = await api.DELETE("/api/accounts/{id}/asb/import", {
    params: { path: { id: acc.id } },
  });
  expect(again.data?.deleted).toBe(0);
  const redone = await (await upload(server.baseURL, acc.id, exportFile(HISTORY))).json();
  expect(redone.imported).toBe(3);
});

test("a body over the size limit is rejected by the server, not the parser", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  // The route carries the shared import body limit (50 MB). Past it the request never reaches
  // the handler at all. Probed over a raw socket, not `fetch` — see `postOversized`.
  const res = await postOversized(server.baseURL, 51 * 1024 * 1024, {
    path: `/api/accounts/${acc.id}/asb/import`,
    contentType: "text/csv",
  });
  expect(res.status).toBe(413);
});

// ---- zips, and one upload spanning several accounts -------------------------------------

/** ASB exports one file per account; a zip is how they arrive together. */
function zipOf(files: Record<string, AsbRow[]>, opts: Record<string, { account: string; ledger?: string }>) {
  const entries: Record<string, string> = {};
  for (const [name, rows] of Object.entries(files)) {
    const o = opts[name];
    entries[name] = new TextDecoder().decode(
      new Uint8Array(exportFile(rows, { account: o.account, ledger: o.ledger }))
    );
  }
  return makeZip(entries);
}

const CHEQUING: AsbRow[] = [
  row("2020/01/20", "2020012001", "EFTPOS", "NEW WORLD METRO QUEEN ST", "EFTPOS", "-12.50"),
  row("2021/06/30", "2021063001", "TFR OUT", "MB TRANSFER", "TO 12-3136- 0000123-51 savings", "-200.00"),
];
const SAVINGS: AsbRow[] = [
  row("2021/06/30", "2021063002", "TFR IN", "MB TRANSFER", "EX 12-3136- 0000123-50 savings", "200.00"),
  row("2022/09/01", "2022090101", "CREDIT", "CREDIT", "CR.INT TO 01/09/2022", "3.21"),
];

/** As with `upload`, opening balances are opted out of unless a spec is about them. */
async function uploadAll(
  baseURL: string,
  body: ArrayBuffer,
  dryRun = false,
  assign?: string,
  openingBalance = false
) {
  const params = new URLSearchParams({
    dry_run: String(dryRun),
    opening_balance: String(openingBalance),
  });
  if (assign) params.set("assign", assign);
  return fetch(`${baseURL}/api/asb/import?${params}`, {
    method: "POST",
    headers: { "Content-Type": "application/zip" },
    body,
  });
}

test("a zip holding one account's export imports through the per-account route", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const zip = zipOf({ "chequing.csv": HISTORY }, { "chequing.csv": { account: "0000123-50" } });

  const r = await (await upload(server.baseURL, acc.id, zip)).json();
  expect(r.imported).toBe(3);
  expect(r.sources).toEqual(["chequing.csv"]);
});

test("several windows of one account in a zip reconcile into one ledger", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const zip = zipOf(
    // The 2021 row is in both files.
    { "a.csv": CHEQUING, "b.csv": [CHEQUING[1], SAVINGS[1]] },
    { "a.csv": { account: "0000123-50" }, "b.csv": { account: "0000123-50" } }
  );

  const r = await (await upload(server.baseURL, acc.id, zip)).json();
  expect(r.rows_total).toBe(3);
  expect(r.imported).toBe(3);
  expect(r.sources.length).toBe(2);
});

test("a multi-account zip is refused by the per-account route, which can't know where each goes", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const zip = zipOf(
    { "chequing.csv": CHEQUING, "savings.csv": SAVINGS },
    { "chequing.csv": { account: "0000123-50" }, "savings.csv": { account: "0000123-51" } }
  );

  const res = await upload(server.baseURL, acc.id, zip);
  expect(res.status).toBe(422);
  const message = (await res.json()).error.message;
  expect(message).toContain("2 different ASB accounts");
  expect(message).toContain("12-3136-0000123-51");
});

test("assigning each export routes a whole bank's worth of accounts in one upload", async ({ api, server }) => {
  const chequing = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const savings = await createAccount(api, "Emergency Fund", "savings", "NZD", { institution: "ASB" });
  const zip = zipOf(
    { "chequing.csv": CHEQUING, "savings.csv": SAVINGS },
    { "chequing.csv": { account: "0000123-50" }, "savings.csv": { account: "0000123-51", ledger: "9500.00" } }
  );

  // Nothing identifies either account yet, so the preview reports them unmatched.
  const preview = await (await uploadAll(server.baseURL, zip, true)).json();
  expect(preview.dry_run).toBe(true);
  expect(preview.exports.length).toBe(2);
  expect(preview.exports.map((x: { asb_account: string }) => x.asb_account)).toEqual([
    "12-3136-0000123-50",
    "12-3136-0000123-51",
  ]);
  expect(preview.exports.every((x: { account_id: number | null }) => x.account_id === null)).toBe(true);
  expect(preview.exports[0].would_import).toBe(2);

  const assign = `12-3136-0000123-50:${chequing.id},12-3136-0000123-51:${savings.id}`;
  const done = await (await uploadAll(server.baseURL, zip, false, assign)).json();
  expect(done.exports.map((x: { account_id: number }) => x.account_id)).toEqual([chequing.id, savings.id]);
  expect(done.exports.map((x: { matched_by: string }) => x.matched_by)).toEqual(["assigned", "assigned"]);
  expect(done.exports.map((x: { imported: number }) => x.imported)).toEqual([2, 2]);
  expect(done.exports[1].account_name).toBe("Emergency Fund");

  // Each account got its own rows, and only its own.
  for (const [id, want] of [
    [chequing.id, "NEW WORLD METRO QUEEN ST"],
    [savings.id, "MB TRANSFER EX 12-3136-0000123-50 savings"],
  ] as const) {
    const { data } = await api.GET("/api/transactions", { params: { query: { account_id: id } } });
    expect(data?.length).toBe(2);
    expect(data?.some((t) => t.description === want)).toBe(true);
  }
});

/** The durable memory: the ids a first import wrote route the next upload on their own. */
test("a second upload routes itself from the previous import", async ({ api, server }) => {
  const chequing = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const savings = await createAccount(api, "Emergency Fund", "savings", "NZD", { institution: "ASB" });
  const zip = zipOf(
    { "chequing.csv": CHEQUING, "savings.csv": SAVINGS },
    { "chequing.csv": { account: "0000123-50" }, "savings.csv": { account: "0000123-51" } }
  );
  const assign = `12-3136-0000123-50:${chequing.id},12-3136-0000123-51:${savings.id}`;
  await uploadAll(server.baseURL, zip, false, assign);

  // No assignments this time.
  const again = await (await uploadAll(server.baseURL, zip, true)).json();
  expect(again.exports.map((x: { account_id: number }) => x.account_id)).toEqual([chequing.id, savings.id]);
  expect(again.exports.map((x: { matched_by: string }) => x.matched_by)).toEqual([
    "previous_import",
    "previous_import",
  ]);
});

test("an account whose name carries the number is matched, and reported as a guess", async ({ api, server }) => {
  const named = await createAccount(api, "Emergency Fund (0000123-51)", "savings", "NZD", {
    institution: "ASB",
  });
  const zip = zipOf({ "savings.csv": SAVINGS }, { "savings.csv": { account: "0000123-51" } });

  const r = await (await uploadAll(server.baseURL, zip, true)).json();
  expect(r.exports[0].account_id).toBe(named.id);
  expect(r.exports[0].matched_by).toBe("account_name");
});

test("a stored account number matches exactly", async ({ api, server }) => {
  const acc = await createAccount(api, "Savings", "savings", "NZD", {
    institution: "ASB",
    metadata: { profile: "depository", account_number: "12-3136-0000123-51" },
  });
  const zip = zipOf({ "savings.csv": SAVINGS }, { "savings.csv": { account: "0000123-51" } });

  const r = await (await uploadAll(server.baseURL, zip, true)).json();
  expect(r.exports[0].account_id).toBe(acc.id);
  expect(r.exports[0].matched_by).toBe("account_number");
});

/**
 * The last-resort tier: an account whose number nothing recorded, matched by the transactions
 * it already holds. A day's tolerance is what makes it work — a feed and the bank's own export
 * routinely disagree about *when* a transaction landed and never about the amount, so these
 * rows are deliberately stamped a day earlier than the export's.
 */
test("an export routes itself by the transactions the account already holds", async ({ api, server }) => {
  const mine = await createAccount(api, "Everyday", "bank", "NZD", { institution: "ASB" });
  const decoy = await createAccount(api, "Other", "savings", "NZD", { institution: "ASB" });

  // Twelve rows, enough to clear the evidence floor a coincidence can't.
  const shared = Array.from({ length: 12 }, (_, i) => ({
    day: 10 + i,
    amount: -(100 + i * 7),
  }));
  for (const { day, amount } of shared) {
    const res = await api.POST("/api/transactions", {
      body: {
        account_id: mine.id,
        // A day earlier than the export says, as a feed stamps it.
        posted_at: `2024-03-${String(day).padStart(2, "0")}`,
        amount_minor: amount,
        description: "from the feed",
      },
    });
    expect(res.response.status, "seed a feed row").toBe(201);
  }

  const rows: AsbRow[] = shared.map(({ day, amount }, i) =>
    row(
      `2024/03/${String(day + 1).padStart(2, "0")}`,
      `2024030${String(i).padStart(3, "0")}`,
      "EFTPOS",
      "SHOP",
      "EFTPOS",
      (amount / 100).toFixed(2)
    )
  );
  const zip = zipOf({ "mystery.csv": rows }, { "mystery.csv": { account: "0000123-77" } });

  const r = await (await uploadAll(server.baseURL, zip, true)).json();
  expect(r.exports[0].account_id).toBe(mine.id);
  expect(r.exports[0].matched_by).toBe("transaction_history");
  expect(r.exports[0].account_id).not.toBe(decoy.id);
});

/** Rate alone is not evidence: two matching rows is a transfer pair, not an identification. */
test("a perfect match on too few rows does not route an export", async ({ api, server }) => {
  const acc = await createAccount(api, "Everyday", "bank", "NZD", { institution: "ASB" });
  const seeded = await api.POST("/api/transactions", {
    body: {
      account_id: acc.id,
      posted_at: "2024-03-10",
      amount_minor: -200_00,
      description: "the only row",
    },
  });
  expect(seeded.response.status, "seed the one row").toBe(201);
  const rows: AsbRow[] = [
    row("2024/03/11", "2024031101", "TFR OUT", "MB TRANSFER", "TO 12-3136- 0000123-51", "-200.00"),
  ];
  const zip = zipOf({ "mystery.csv": rows }, { "mystery.csv": { account: "0000123-77" } });

  const r = await (await uploadAll(server.baseURL, zip, true)).json();
  expect(r.exports[0].account_id).toBe(null);
  expect(r.exports[0].matched_by).toBe(null);
});

test("an unmatched export is reported and left alone, while its neighbours import", async ({ api, server }) => {
  const chequing = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const zip = zipOf(
    { "chequing.csv": CHEQUING, "mystery.csv": SAVINGS },
    { "chequing.csv": { account: "0000123-50" }, "mystery.csv": { account: "0999999-99" } }
  );

  const r = await (await uploadAll(server.baseURL, zip, false, `12-3136-0000123-50:${chequing.id}`)).json();
  expect(r.exports[0].imported).toBe(2);
  expect(r.exports[1].account_id).toBe(null);
  expect(r.exports[1].imported).toBe(0);
  expect(r.exports[1].warnings.some((w: string) => w.includes("no account was matched"))).toBe(true);

  const { data } = await api.GET("/api/transactions", {});
  expect(data?.length).toBe(2);
});

test("each account's own cutover applies, not one for the whole upload", async ({ api, server }) => {
  const chequing = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const savings = await createAccount(api, "Savings", "savings", "NZD", { institution: "ASB" });
  // Only the chequing account has a live feed, reaching back to 2021.
  await feed(api, chequing.id, "date,amount,description,external_id\n2021-01-01,-5.00,Feed row,f1\n");
  const zip = zipOf(
    { "chequing.csv": CHEQUING, "savings.csv": SAVINGS },
    { "chequing.csv": { account: "0000123-50" }, "savings.csv": { account: "0000123-51" } }
  );

  const r = await (
    await uploadAll(server.baseURL, zip, false, `12-3136-0000123-50:${chequing.id},12-3136-0000123-51:${savings.id}`)
  ).json();
  expect(r.exports[0].cutover).toBe("2021-01-01");
  expect(r.exports[0].held_back).toBe(1);
  expect(r.exports[0].imported).toBe(1);
  expect(r.exports[1].cutover).toBe(null);
  expect(r.exports[1].held_back).toBe(0);
  expect(r.exports[1].imported).toBe(2);
});

test("a malformed assignment is refused before anything is read", async ({ api, server }) => {
  await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const zip = zipOf({ "chequing.csv": CHEQUING }, { "chequing.csv": { account: "0000123-50" } });

  const res = await uploadAll(server.baseURL, zip, false, "12-3136-0000123-50:not-a-number");
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("is not an account id");
});

test("a zip with no exports in it is refused", async ({ api, server }) => {
  await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const res = await uploadAll(server.baseURL, makeZip({ "notes.txt": "hello" }));
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("no .csv files");
});


/**
 * The property the whole preview rests on, and the one that's easy to lose: how many rows an
 * export contributes depends on the *target account's* cutover, so an unassigned preview can
 * only report the whole file. Once assigned, the preview and the commit must agree exactly —
 * which is why the UI re-previews whenever a selection changes.
 */
test("an assigned preview reports exactly what the commit then does", async ({ api, server }) => {
  const chequing = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const savings = await createAccount(api, "Savings", "savings", "NZD", { institution: "ASB" });
  await feed(api, chequing.id, "date,amount,description,external_id\n2021-01-01,-5.00,Feed row,f1\n");
  const zip = zipOf(
    { "chequing.csv": CHEQUING, "savings.csv": SAVINGS },
    { "chequing.csv": { account: "0000123-50" }, "savings.csv": { account: "0000123-51" } }
  );
  const assign = `12-3136-0000123-50:${chequing.id},12-3136-0000123-51:${savings.id}`;

  // Unassigned: no account, so no cutover to apply — the whole file.
  const blind = await (await uploadAll(server.baseURL, zip, true)).json();
  expect(blind.exports[0].account_id).toBe(null);
  expect(blind.exports[0].would_import).toBe(2);
  expect(blind.exports[0].cutover).toBe(null);

  // Assigned: the chequing account's feed cuts its contribution down.
  const preview = await (await uploadAll(server.baseURL, zip, true, assign)).json();
  expect(preview.exports[0].cutover).toBe("2021-01-01");
  expect(preview.exports[0].would_import).toBe(1);

  const done = await (await uploadAll(server.baseURL, zip, false, assign)).json();
  for (const [i, x] of done.exports.entries()) {
    expect(x.imported).toBe(preview.exports[i].would_import);
    expect(x.held_back).toBe(preview.exports[i].held_back);
    expect(x.cutover).toBe(preview.exports[i].cutover);
    expect(x.account_id).toBe(preview.exports[i].account_id);
  }
});

// ---- malformed and hostile uploads ------------------------------------------------------

/**
 * Both ASB routes take an arbitrary uploaded file, so "the parser panicked", "the process ran
 * out of memory" and "the request hung" are all failures of the same kind: **the request must
 * fail, not the server.** The per-file parse rules are unit tested in `sure_providers::asb`;
 * these check the endpoints behave, and that the ceilings in `sure_providers::zipfile` hold.
 */
const HOSTILE_BODIES: [string, () => ArrayBuffer][] = [
  ["empty", () => new ArrayBuffer(0)],
  ["html", () => new TextEncoder().encode("<html>nope</html>").buffer as ArrayBuffer],
  ["a bare zip signature", () => new Uint8Array([0x50, 0x4b, 0x03, 0x04, 0, 0, 0, 0]).buffer as ArrayBuffer],
  ["random bytes", () => new Uint8Array(Array.from({ length: 4096 }, (_, i) => (i * 37) % 256)).buffer as ArrayBuffer],
  ["nothing but NULs", () => new Uint8Array(1024).buffer as ArrayBuffer],
  ["a zip of a zip", () => makeZip({ "inner.zip": new Uint8Array(makeZip({ "x.csv": "junk" })) })],
  ["an export with a header but a garbage body", () =>
    new TextEncoder().encode(
      "Bank 12; Branch 3136; Account 0000123-50 (X)\r\nDate,Unique Id,Tran Type,Cheque Number,Payee,Memo,Amount\r\n\0\0\0\0\r\n"
    ).buffer as ArrayBuffer],
];

test("hostile bodies are refused by the per-account route, and the server survives", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  for (const [name, make] of HOSTILE_BODIES) {
    const res = await upload(server.baseURL, acc.id, make());
    expect(res.status, name).toBe(422);
    expect((await res.json()).error.message, name).toBeTruthy();
  }
  const { data } = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
  expect(data?.length).toBe(0);
  expect((await api.GET("/api/health", {})).response.status).toBe(200);
});

test("hostile bodies are refused by the multi-account route too", async ({ api, server }) => {
  await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  for (const [name, make] of HOSTILE_BODIES) {
    const res = await uploadAll(server.baseURL, make());
    expect(res.status, name).toBe(422);
  }
  expect((await api.GET("/api/health", {})).response.status).toBe(200);
});

test("a zip bomb is refused without expanding it", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  // 20 MB of zeroes deflates to almost nothing. The HTTP body limit bounds what arrives;
  // only a ceiling on what it expands *to* stops this.
  const bomb = makeZip({ "bomb.csv": new Uint8Array(20 * 1024 * 1024) }, { deflate: true });
  expect(bomb.byteLength).toBeLessThan(200_000);

  for (const res of [
    await upload(server.baseURL, acc.id, bomb),
    await uploadAll(server.baseURL, bomb),
  ]) {
    expect(res.status).toBe(422);
    expect((await res.json()).error.message).toMatch(/over the limit|expands/);
  }
});

test("many entries that each fit still can't add up past the upload ceiling", async ({ api, server }) => {
  await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const files: Record<string, Uint8Array> = {};
  // Each 15 MB — under the per-entry ceiling — but 75 MB together, over the upload's.
  for (let i = 0; i < 5; i++) files[`pad${i}.csv`] = new Uint8Array(15 * 1024 * 1024);
  const res = await uploadAll(server.baseURL, makeZip(files, { deflate: true }));
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toMatch(/expands|over the limit/);
});

test("an entry that under-declares its size is still capped", async ({ api, server }) => {
  await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const zip = new Uint8Array(makeZip({ "liar.csv": new Uint8Array(20 * 1024 * 1024) }, { deflate: true }));
  // Rewrite both size fields to claim a handful of bytes. The read has to bound itself.
  for (const sig of [0x04034b50, 0x02014b50]) {
    for (let i = 0; i + 4 <= zip.length; i++) {
      const v = zip[i] | (zip[i + 1] << 8) | (zip[i + 2] << 16) | (zip[i + 3] << 24);
      if (v >>> 0 !== sig >>> 0) continue;
      const at = sig === 0x04034b50 ? i + 22 : i + 24;
      new DataView(zip.buffer).setUint32(at, 8, true);
    }
  }
  const res = await uploadAll(server.baseURL, zip.buffer as ArrayBuffer);
  // Either the cap trips or the truncated body fails to parse — never an unbounded read.
  expect(res.status).toBe(422);
});

test("a zip with far too many exports in it is refused", async ({ api, server }) => {
  await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const files: Record<string, string> = {};
  for (let i = 0; i < 200; i++) files[`e${i}.csv`] = "junk";
  const res = await uploadAll(server.baseURL, makeZip(files));
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("at most");
});

test("a massive row count is refused rather than imported", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  // Past the 100,000-row ceiling the parser carries for a whole upload.
  const rows: AsbRow[] = Array.from({ length: 120_000 }, (_, i) =>
    row("2020/01/20", `2020012${String(i).padStart(4, "0")}`, "EFTPOS", "SHOP", "EFTPOS", "-1.00")
  );
  const res = await upload(server.baseURL, acc.id, exportFile(rows));
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("too many rows");

  const { data } = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
  expect(data?.length).toBe(0);
});

test("path-traversal entry names are inert — nothing is ever written to disk", async ({ api, server }) => {
  const chequing = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const zip = makeZip({
    "../../../../etc/passwd.csv": new TextDecoder().decode(
      new Uint8Array(exportFile(HISTORY))
    ),
  });
  const r = await (await uploadAll(server.baseURL, zip, false, `12-3136-0000123-50:${chequing.id}`)).json();
  // The name is only ever a label in a message; the rows import as any other file's would.
  expect(r.exports[0].imported).toBe(3);
  expect(r.exports[0].sources[0]).toContain("passwd.csv");
});

test("a giant single field is refused rather than parsed", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const giant = "x".repeat(2 * 1024 * 1024);
  const res = await upload(
    server.baseURL,
    acc.id,
    exportFile([row("2020/01/20", "2020012001", "EFTPOS", giant, "EFTPOS", "-1.00")])
  );
  // Accepted (it's only a long description) or refused — but bounded either way, and quick.
  expect([200, 422]).toContain(res.status);
  expect((await api.GET("/api/health", {})).response.status).toBe(200);
});

test("a large but honest upload imports, and stays fast", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  // Seven years of a busy everyday account, which is what this feature is actually for.
  const rows: AsbRow[] = Array.from({ length: 3000 }, (_, i) =>
    row("2020/01/20", `2020012${String(i).padStart(4, "0")}`, "EFTPOS", `SHOP ${i}`, "EFTPOS", "-1.00")
  );
  const started = Date.now();
  const r = await (await upload(server.baseURL, acc.id, exportFile(rows))).json();
  expect(r.imported).toBe(3000);
  expect(Date.now() - started).toBeLessThan(30_000);
});

// ---- the opening balance ----------------------------------------------------------------

/**
 * Why this exists: the balance reconstruction reads an account as 0 before its earliest
 * transaction, so imported history otherwise starts from nothing — the account appears out of
 * thin air at whatever its first day's movements leave behind, rather than at the balance it
 * actually held. The figure is the closing balance ASB states, less every movement in the file.
 *
 * `HISTORY` closes at 100.00 and its rows sum to −12.50 − 200.00 + 1500.00 = 1287.50, so the
 * account must have held 100.00 − 1287.50 = −1204.98 before 2020-01-20.
 */
const OPENING_MINOR = 100_00 - (-1250 - 200_00 + 150_000);

test("records the opening balance the export implies, by default", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const r = await (await upload(server.baseURL, acc.id, exportFile(HISTORY), false, true)).json();

  expect(r.implied_opening_minor).toBe(OPENING_MINOR);
  expect(r.opening_balance_minor).toBe(OPENING_MINOR);
  // The day before the first row, so the account is 0 before it and right from it onward.
  expect(r.opening_balance_as_of).toBe("2020-01-19");
  expect(r.imported).toBe(4);

  const { data: txns } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  const opening = txns!.find((t) => t.description === "Opening balance")!;
  expect(opening.amount_minor).toBe(OPENING_MINOR);
  expect(opening.posted_at).toBe("2020-01-19T12:00:00+00:00");
  // A one-off: counted towards balances, never towards income.
  expect(opening.is_one_off).toBe(true);
  expect(opening.category_id).toBe(null);
  // Tagged like the rest, so the undo takes it too.
  expect(opening.provider).toBe(`asb#${acc.id}`);
  expect(opening.external_id).toBe("asb:12-3136-0000123-50:opening");
});

/** The point of the whole thing: the reported history has to be right from the first day. */
test("with the opening balance the account's history reconstructs correctly", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  // The balance Sure holds today, which the reconstruction anchors on.
  await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: acc.id } },
    body: { as_of: "2026-08-03", value_minor: 100_00 },
  });
  await upload(server.baseURL, acc.id, exportFile(HISTORY), false, true);

  const at = async (to: string) => {
    const { data } = await api.GET("/api/reports/balances", { params: { query: { from: "2019-01-01", to } } });
    return data!.accounts.find((a) => a.account_id === acc.id)?.value_minor;
  };
  // Before the opening row the account didn't exist yet.
  expect(await at("2020-01-18")).toBe(0);
  // On the opening date it holds exactly the worked-back figure …
  expect(await at("2020-01-19")).toBe(OPENING_MINOR);
  // … then each movement carries it forward to the balance it closes at.
  expect(await at("2020-01-20")).toBe(OPENING_MINOR - 1250);
  expect(await at("2021-06-30")).toBe(OPENING_MINOR - 1250 - 200_00);
  expect(await at("2026-08-03")).toBe(100_00);
});

/**
 * An opening balance moves the account's value without being money earned or spent, so it must
 * not shift a single spend or income figure. Asserted by importing the same file twice over —
 * once without it, once with — and requiring identical reports.
 */
test("the opening balance changes no spend or income figure", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const totals = async () => {
    const { data } = await api.GET("/api/reports/category-breakdown", {
      params: { query: { from: "2019-01-01", to: "2026-12-31" } },
    });
    return {
      income: (data!.income ?? []).reduce((n, c) => n + c.total_minor, 0),
      expense: (data!.expense ?? []).reduce((n, c) => n + c.total_minor, 0),
    };
  };

  await upload(server.baseURL, acc.id, exportFile(HISTORY), false, false);
  const before = await totals();
  expect(before.income + before.expense).not.toBe(0);

  await api.DELETE("/api/accounts/{id}/asb/import", { params: { path: { id: acc.id } } });
  const r = await (await upload(server.baseURL, acc.id, exportFile(HISTORY), false, true)).json();
  expect(r.opening_balance_minor).toBe(OPENING_MINOR);

  expect(await totals()).toEqual(before);
});

test("opting out records no opening balance", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const r = await (await upload(server.baseURL, acc.id, exportFile(HISTORY), false, false)).json();
  // The arithmetic is still reported — it's the recording that was declined.
  expect(r.implied_opening_minor).toBe(OPENING_MINOR);
  expect(r.opening_balance_minor).toBe(null);
  expect(r.imported).toBe(3);

  const { data: txns } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  expect(txns!.some((t) => t.description === "Opening balance")).toBe(false);
});

test("a dry run reports the opening balance without writing it", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const preview = await (await upload(server.baseURL, acc.id, exportFile(HISTORY), true, true)).json();
  expect(preview.opening_balance_minor).toBe(OPENING_MINOR);
  // Counted in what the commit would write, so the preview's figure is the real one.
  expect(preview.would_import).toBe(4);
  const { data } = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
  expect(data?.length).toBe(0);

  const done = await (await upload(server.baseURL, acc.id, exportFile(HISTORY), false, true)).json();
  expect(done.imported).toBe(preview.would_import);
  expect(done.opening_balance_minor).toBe(preview.opening_balance_minor);
  expect(done.opening_balance_as_of).toBe(preview.opening_balance_as_of);
});

/**
 * The guard that matters: an "opening" balance placed after the ledger already starts is not
 * an opening balance, it's a large invented movement in the middle of the history.
 */
test("an account with earlier history gets no opening balance, and is told why", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  await api.POST("/api/transactions", {
    body: {
      account_id: acc.id,
      posted_at: "2015-01-01",
      amount_minor: -999,
      description: "Ancient history",
      is_one_off: false,
    },
  });

  const r = await (await upload(server.baseURL, acc.id, exportFile(HISTORY), false, true)).json();
  expect(r.opening_balance_minor).toBe(null);
  expect(r.imported).toBe(3);
  expect(r.warnings.some((w: string) => w.includes("already has transactions from before"))).toBe(true);
});

test("re-uploading doesn't add a second opening balance", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  await upload(server.baseURL, acc.id, exportFile(HISTORY), false, true);
  const again = await (await upload(server.baseURL, acc.id, exportFile(HISTORY), false, true)).json();
  expect(again.imported).toBe(0);
  expect(again.skipped).toBe(4);

  const { data: txns } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  expect(txns!.filter((t) => t.description === "Opening balance").length).toBe(1);
});

test("undo removes the opening balance along with the rest", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  await upload(server.baseURL, acc.id, exportFile(HISTORY), false, true);
  const undo = await api.DELETE("/api/accounts/{id}/asb/import", { params: { path: { id: acc.id } } });
  expect(undo.data?.deleted).toBe(4);
  const { data } = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
  expect(data?.length).toBe(0);
});

test("an export with no closing balance records no opening balance", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const text = new TextDecoder().decode(new Uint8Array(exportFile(HISTORY)));
  const without = new TextEncoder().encode(
    text.replace("Ledger Balance : 100.00 as of 20260803\r\n", "")
  ).buffer as ArrayBuffer;

  const r = await (await upload(server.baseURL, acc.id, without, false, true)).json();
  expect(r.implied_opening_minor).toBe(null);
  expect(r.opening_balance_minor).toBe(null);
  expect(r.imported).toBe(3);
});

test("each account in a multi-account upload gets its own opening balance", async ({ api, server }) => {
  const chequing = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const savings = await createAccount(api, "Savings", "savings", "NZD", { institution: "ASB" });
  const zip = zipOf(
    { "chequing.csv": CHEQUING, "savings.csv": SAVINGS },
    { "chequing.csv": { account: "0000123-50" }, "savings.csv": { account: "0000123-51", ledger: "9500.00" } }
  );
  const assign = `12-3136-0000123-50:${chequing.id},12-3136-0000123-51:${savings.id}`;

  const r = await (await uploadAll(server.baseURL, zip, false, assign, true)).json();
  // 100.00 − (−12.50 − 200.00) for the first; 9500.00 − (200.00 + 3.21) for the second.
  expect(r.exports[0].opening_balance_minor).toBe(100_00 - (-1250 - 200_00));
  expect(r.exports[1].opening_balance_minor).toBe(950_000 - (200_00 + 321));
  expect(r.exports.map((x: { imported: number }) => x.imported)).toEqual([3, 3]);
  expect(r.exports[0].opening_balance_as_of).toBe("2020-01-19");
  expect(r.exports[1].opening_balance_as_of).toBe("2021-06-29");
});
