import { test, expect } from "../fixtures";
import { createAccount, makeZip, postOversized } from "../helpers";

// The behaviours that belong to the *pipeline* rather than to any one source — the ones that only
// exist because there is now one import endpoint instead of four.
//
// What each source makes of its own file is `asb.spec.ts`, `student-loan.spec.ts` and
// `brokerage.spec.ts`, and the parsing itself is unit tested beside each parser. What is here is
// the shared spine: recognising the file, overriding that guess, previewing before writing,
// undoing afterwards, and the ceilings and refusals that apply whatever arrived.
//
// It matters that these are asserted per source and not just once. Before the unification the
// same capability was present on one source and absent on the next — only ASB could preview, only
// ASB could be undone — and a per-source test would have found that a passing state of affairs.

// ---- fixtures, one per source -------------------------------------------------------------

const ASB_ACCOUNT = "12-3136-0000123-50";

/** An ASB export: the preamble ASB really writes, then a header, then rows. */
function asbCsv(account = "0000123-50", closing = "95.00"): ArrayBuffer {
  const text =
    "Created date / time : 3 August 2026 / 16:27:53\r\n" +
    `Bank 12; Branch 3136; Account ${account} (Streamline)\r\n` +
    "From date 20260101\r\nTo date 20260803\r\n" +
    `Ledger Balance : ${closing} as of 20260803\r\n\r\n` +
    "Date,Unique Id,Tran Type,Cheque Number,Payee,Memo,Amount\r\n" +
    '2026/07/27,2026072701,EFTPOS,,"COUNTDOWN QUEEN ST","EFTPOS",-5.00\r\n' +
    '2026/07/28,2026072801,D/C,,"D/C FROM ACME CORP","ACME CORP",100.00\r\n';
  return new TextEncoder().encode(text).buffer as ArrayBuffer;
}

const PLAIN_CSV = new TextEncoder().encode(
  "date,amount,description,merchant\n2025-03-01,-12.50,Coffee,Kaffee\n2025-03-02,42.00,Refund,\n"
).buffer as ArrayBuffer;

// A minimal `.xlsx` — itself a zip of XML parts. Kept in step with `student-loan.spec.ts`'s copy;
// duplicated rather than shared because that file's version carries the malformed-grid variants
// this one has no use for.
const SLS = "012-345-678-SLS004";
const CONTENT_TYPES = `<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>`;
const ROOT_RELS = `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>`;
const WORKBOOK = `<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Transactions" sheetId="1" r:id="rId1"/></sheets></workbook>`;
const WORKBOOK_RELS = `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>`;

function sheetXml(grid: string[][]): string {
  const cell = (v: string, c: number, r: number) =>
    `<c r="${String.fromCharCode(65 + c)}${r}" t="inlineStr"><is><t>${v.replace(/&/g, "&amp;").replace(/</g, "&lt;")}</t></is></c>`;
  return (
    `<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>` +
    grid
      .map((cells, i) => `<row r="${i + 1}">${cells.map((v, c) => cell(v, c, i + 1)).join("")}</row>`)
      .join("") +
    `</sheetData></worksheet>`
  );
}

function myirXlsx(rows: [string, string, string][] = [["2025-04-14", "Repayment deduction", "-400.00"]]) {
  return makeZip({
    "[Content_Types].xml": CONTENT_TYPES,
    "_rels/.rels": ROOT_RELS,
    "xl/workbook.xml": WORKBOOK,
    "xl/_rels/workbook.xml.rels": WORKBOOK_RELS,
    "xl/worksheets/sheet1.xml": sheetXml([
      ["Account ID:", SLS],
      ["From:", "2024-07-31"],
      ["To:", "2026-07-31"],
      ["Period ending", "Account type", "Date", "Transaction", "Amount"],
      ...rows.map(([date, txn, amount]) => ["2026-03-31", "Student loan", date, txn, amount]),
    ]),
  });
}

/**
 * A Sharesies export: the three files the parser reads, in the shapes it really reads them in.
 *
 * Kept deliberately close to `brokerage.spec.ts`'s fixture — `$quantum` timestamps, `trades` with
 * a contract note, a `corporate_action_v2` dividend — because inventing a plausible-looking shape
 * instead produced a fixture the parser rejected, and a test asserting the pipeline would have
 * been reporting a broken fixture.
 */
function sharesiesZip(): ArrayBuffer {
  return makeZip({
    "sharesies-export/lookup.json": JSON.stringify({
      "fund-zz": { symbol: "ZZTEST", name: "Test Instrument", exchange: "NZX", currency: "NZD" },
    }),
    "sharesies-export/wallet-transactions.json": JSON.stringify([
      {
        amount: "-100.00",
        currency: "nzd",
        description: "Withdrawal",
        reason: "holding funds for withdrawal",
        key: "wallet-withdrawal-1",
        timestamp: { $quantum: Date.parse("2026-01-05T00:00:00Z") },
        detail: { type: "withdrawal" },
      },
    ]),
    "sharesies-export/activity.json": JSON.stringify([
      {
        id: "order-1",
        type: "buy",
        state: "fulfilled",
        fund_id: "fund-zz",
        total_transaction_fee: "5.00",
        trades: [
          {
            contract_note_number: "cn1",
            trade_datetime: { $quantum: Date.parse("2026-01-02T00:00:00Z") },
            share_price: "1.50",
            volume: "100",
          },
        ],
      },
    ]),
  });
}

// ---- posting -----------------------------------------------------------------------------

type Query = { dry_run?: boolean; assign?: string; source?: string; opening_balance?: boolean };

async function post(baseURL: string, body: ArrayBuffer, q: Query = {}) {
  const params = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) if (v !== undefined) params.set(k, String(v));
  return fetch(`${baseURL}/api/import?${params}`, {
    method: "POST",
    headers: { "Content-Type": "application/octet-stream" },
    body,
  });
}

const ok = async (res: Response) => {
  expect(res.status, await res.clone().text()).toBe(200);
  return res.json();
};

/**
 * Answer the price lookup a Sharesies import's backfill makes, for every test in this file.
 *
 * The import hands its valuation history walk back to the transport as a `FollowUp`, which spawns
 * it and answers immediately — so every Sharesies upload below starts an outbound chart request
 * that nothing here is about. Left unstubbed it is a replay miss the suite now fails a test over
 * (see `failOnUnstubbedRequests` in ../fixtures.ts), and rightly: an unanswered upstream sends the
 * code under test down an error path nobody asked for.
 *
 * A chart with no `timestamp` array is the feed's own way of saying it has nothing for the window,
 * so the backfill runs its normal path and stores nothing. Same stub, same reasoning, as
 * `brokerage.spec.ts`'s.
 */
test.beforeEach(async ({ testproxy }) => {
  await testproxy.stub({
    upstream: "yahoo_finance",
    method: "GET",
    path_pattern: "^/v8/finance/chart/ZZTEST\\.NZ$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: JSON.stringify({
      chart: {
        result: [
          {
            meta: { currency: "NZD", gmtoffset: 12 * 3600 },
            // Not optional in `ChartResult`, so an empty one still has to be here.
            indicators: { quote: [{ close: [] }] },
          },
        ],
      },
    }),
  });
});

// ---- what the file is --------------------------------------------------------------------

/**
 * Every source is recognised from its own bytes, with no hint from the caller — no filename, no
 * content type, no per-source URL. This is what replaced four endpoints: the person uploading no
 * longer has to know which importer their file belongs to, or find the button for it.
 */
test("each source is recognised from the file alone", async ({ server }) => {
  for (const [body, source] of [
    [asbCsv(), "asb_csv"],
    [myirXlsx(), "myir_sls"],
    [sharesiesZip(), "sharesies_zip"],
    [PLAIN_CSV, "csv_upload"],
  ] as const) {
    const result = await ok(await post(server.baseURL, body, { dry_run: true }));
    expect(result.source, `detected source for ${source}`).toBe(source);
  }
});

/**
 * A bank export also has `date` and `amount` columns, so the plain CSV reader *could* claim it.
 * Detection asks the specific sources first for exactly that reason: read as a plain CSV, an ASB
 * export would import with no cutover, no opening balance and no account routing — and report
 * success.
 */
test("a bank export is not claimed by the plain CSV reader", async ({ server }) => {
  const result = await ok(await post(server.baseURL, asbCsv(), { dry_run: true }));
  expect(result.source).toBe("asb_csv");
  expect(result.items[0].source_account).toBe(ASB_ACCOUNT);
});

test("a file that is no export at all is refused, and says so usefully", async ({ api, server }) => {
  for (const text of ["", "<html><body>nope</body></html>", "not,columns\nwe,know\n"]) {
    const res = await post(server.baseURL, new TextEncoder().encode(text).buffer as ArrayBuffer);
    expect(res.status).toBe(422);
    expect((await res.json()).error.message).toContain("doesn't look like any export Sure can read");
  }
  const health = await api.GET("/api/health", {});
  expect(health.response.status).toBe(200);
});

/**
 * The escape hatch, and the reason it can't be a silent fallback: a caller naming a source is
 * overriding the sniff on purpose. Ignoring a typo would import the file the one way they said
 * not to.
 */
test("the caller can name the source, and a name that isn't one is refused", async ({ server }) => {
  const forced = await ok(
    await post(server.baseURL, PLAIN_CSV, { dry_run: true, source: "csv_upload" })
  );
  expect(forced.source).toBe("csv_upload");

  const res = await post(server.baseURL, PLAIN_CSV, { source: "sharesies" });
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("asb_csv");
});

// ---- preview, for every source -----------------------------------------------------------

/**
 * The capability myIR and Sharesies did not have. Their old endpoints wrote on the only request
 * they took, so "a few thousand rows into real money data" had no look-before step at all.
 *
 * Asserted as *equality* between the preview's `would_import` and the commit's `imported`, not
 * merely that a preview returns something: a preview that describes an import other than the one
 * that follows is worse than no preview.
 */
for (const [name, body, assign] of [
  ["an ASB export", asbCsv(), (id: number) => `${ASB_ACCOUNT}:${id}`],
  ["a myIR export", myirXlsx(), (id: number) => `${SLS}:${id}`],
  ["a Sharesies export", sharesiesZip(), (id: number) => `sharesies:${id}`],
  ["a plain CSV", PLAIN_CSV, (id: number) => `csv:${id}`],
] as const) {
  const kind = name.includes("myIR")
    ? "student_loan"
    : name.includes("Sharesies")
      ? "brokerage"
      : "bank";

  test(`${name} previews exactly what the commit then imports`, async ({ api, server }) => {
    const acc = await createAccount(api, "Target", kind, "NZD", { institution: "ASB" });
    const q = { assign: assign(acc.id), opening_balance: false };

    const preview = await ok(await post(server.baseURL, body, { ...q, dry_run: true }));
    expect(preview.dry_run).toBe(true);
    expect(preview.items[0].imported).toBe(0);
    const promised = preview.items[0].would_import;
    expect(promised).toBeGreaterThan(0);

    // Nothing was written by the preview.
    const before = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
    expect(before.data?.length).toBe(0);

    const commit = await ok(await post(server.baseURL, body, q));
    expect(commit.dry_run).toBe(false);
    expect(commit.items[0].imported + commit.items[0].skipped).toBe(promised);
  });
}

// ---- undo, for every source --------------------------------------------------------------

/**
 * The other capability only ASB had. Note what it deliberately is: per (account, source), not per
 * upload. Two overlapping uploads share their content-derived ids, so the second one's rows were
 * *skipped* rather than written — there is nothing of it on its own left to take back.
 */
for (const [name, body, source, assign] of [
  ["ASB", asbCsv(), "asb_csv", (id: number) => `${ASB_ACCOUNT}:${id}`],
  ["myIR", myirXlsx(), "myir_sls", (id: number) => `${SLS}:${id}`],
  ["Sharesies", sharesiesZip(), "sharesies_zip", (id: number) => `sharesies:${id}`],
] as const) {
  const kind = source === "myir_sls" ? "student_loan" : source === "sharesies_zip" ? "brokerage" : "bank";

  test(`a ${name} import can be taken back out`, async ({ api, server }) => {
    const acc = await createAccount(api, "Target", kind, "NZD", { institution: "ASB" });
    const imported = await ok(
      await post(server.baseURL, body, { assign: assign(acc.id), opening_balance: false })
    );
    expect(imported.items[0].imported).toBeGreaterThan(0);

    const undo = await api.DELETE("/api/import/{account_id}/{source}", {
      params: { path: { account_id: acc.id, source } },
    });
    expect(undo.response.status).toBe(200);
    expect(undo.data!.deleted).toBe(imported.items[0].imported);

    const left = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
    expect(left.data?.length).toBe(0);
  });
}

test("an undo naming a source that isn't one is refused", async ({ api, server }) => {
  const acc = await createAccount(api, "Everyday", "bank", "NZD", { institution: "ASB" });
  const res = await fetch(`${server.baseURL}/api/import/${acc.id}/asb`, { method: "DELETE" });
  expect(res.status).toBe(422);
});

// ---- assignments -------------------------------------------------------------------------

/**
 * An assignment is the same statement of intent the per-account routes expressed with a path, so
 * it is checked the same way and just as early — before a byte is read. Falling through to the
 * other routing tiers would put the rows somewhere the caller didn't ask for, or nowhere, and
 * report success either way.
 */
test("an assignment to a wrong-kind account is refused before anything is read", async ({
  api,
  server,
}) => {
  const home = await createAccount(api, "Family home", "real_estate");
  const res = await post(server.baseURL, asbCsv(), { assign: `${ASB_ACCOUNT}:${home.id}` });
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("can't be imported into Family home");
});

test("an assignment to an account that does not exist is a 404", async ({ server }) => {
  const res = await post(server.baseURL, asbCsv(), { assign: `${ASB_ACCOUNT}:987654` });
  expect(res.status).toBe(404);
});

/**
 * The tier that replaced having the account in the URL, and the line drawn through it: it fires
 * for a source whose export cannot name a Sure account (a loan, a brokerage) and *not* for a bank
 * export, which carries an account number the identifier tiers already tried. If those found
 * nothing, the honest reading is that the account isn't in Sure yet — not that it must be the one
 * that is.
 */
test("a lone loan export finds the only loan, while a bank export will not guess", async ({
  api,
  server,
}) => {
  const loan = await createAccount(api, "Student loan", "student_loan");
  const routed = await ok(await post(server.baseURL, myirXlsx(), { dry_run: true }));
  expect(routed.items[0].account_id).toBe(loan.id);
  expect(routed.items[0].matched_by).toBe("only_candidate");

  // One bank account, and an export whose number matches nothing about it.
  await createAccount(api, "Everyday", "bank", "NZD", { institution: "ASB" });
  const unrouted = await ok(await post(server.baseURL, asbCsv(), { dry_run: true }));
  expect(unrouted.items[0].account_id).toBeNull();
  expect(unrouted.items[0].warnings.join(" ")).toContain("no account was matched");
});

// ---- several files in one upload ---------------------------------------------------------

/**
 * What the browser sends when someone picks several files at once: it wraps them in a stored zip
 * (`packages/web/src/lib/zip.ts`) rather than making N requests, because that is the only shape
 * whose cross-file checks can run — a myIR gap check needs every export in hand.
 *
 * Built here the way the browser builds it, uncompressed and in order, so this asserts the bytes
 * that path really produces are the bytes the server reads.
 */
test("several files arrive as one stored zip and are read together", async ({ api, server }) => {
  const chequing = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const savings = await createAccount(api, "Savings", "savings", "NZD", { institution: "ASB" });

  const zip = makeZip({
    "chequing.csv": new Uint8Array(asbCsv("0000123-50", "95.00")),
    "savings.csv": new Uint8Array(asbCsv("0000123-51", "500.00")),
  });
  const result = await ok(
    await post(server.baseURL, zip, {
      assign: `12-3136-0000123-50:${chequing.id},12-3136-0000123-51:${savings.id}`,
      opening_balance: false,
    })
  );

  expect(result.items).toHaveLength(2);
  expect(result.items.map((x: { account_id: number }) => x.account_id)).toEqual([
    chequing.id,
    savings.id,
  ]);
  // Each file is named against the account it went to, which is the only way to tell afterwards
  // which of your downloads landed where.
  expect(result.items[0].sources).toEqual(["chequing.csv"]);
  expect(result.items[1].sources).toEqual(["savings.csv"]);
  for (const item of result.items) expect(item.imported).toBe(2);
});

// ---- the ceilings ------------------------------------------------------------------------

/**
 * One body limit for every source now, so there is nothing for four of them to disagree about —
 * and it is the *server* that refuses, before the handler or any parser sees a byte. Probed over a
 * raw socket, not `fetch`: see `postOversized`.
 */
test("a body over the size limit is refused by the server, whatever it claims to be", async ({
  server,
}) => {
  for (const contentType of ["text/csv", "application/zip"]) {
    const res = await postOversized(server.baseURL, 51 * 1024 * 1024, {
      path: "/api/import",
      contentType,
    });
    expect(res.status, contentType).toBe(413);
  }
});

// ---- the log -----------------------------------------------------------------------------

/**
 * Recorded per (upload, account), so "how much of this account came from an export, and how far
 * back does it reach" is a read rather than an inference. Two panels used to answer it by fetching
 * ten thousand transactions and filtering client-side on the provider tag.
 */
test("every commit is recorded, and a dry run is not", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const q = { assign: `${ASB_ACCOUNT}:${acc.id}`, opening_balance: false };

  await ok(await post(server.baseURL, asbCsv(), { ...q, dry_run: true }));
  expect((await api.GET("/api/imports", {})).data).toEqual([]);

  await ok(await post(server.baseURL, asbCsv(), q));
  const { data } = await api.GET("/api/imports", {
    params: { query: { account_id: acc.id } },
  });
  expect(data).toHaveLength(1);
  expect(data![0].source).toBe("asb_csv");
  expect(data![0].source_account).toBe(ASB_ACCOUNT);
  expect(data![0].imported).toBe(2);
  expect(data![0].covered_from).toBe("2026-07-27");
  expect(data![0].covered_to).toBe("2026-07-28");
});

/**
 * The log is a log: it survives an undo, because the import did happen. The panel's heading says
 * "Import history" for this reason and not "Imported here".
 */
test("the log records a re-import and survives an undo", async ({ api, server }) => {
  const acc = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const q = { assign: `${ASB_ACCOUNT}:${acc.id}`, opening_balance: false };

  await ok(await post(server.baseURL, asbCsv(), q));
  await ok(await post(server.baseURL, asbCsv(), q));
  await api.DELETE("/api/import/{account_id}/{source}", {
    params: { path: { account_id: acc.id, source: "asb_csv" } },
  });

  const { data } = await api.GET("/api/imports", {
    params: { query: { account_id: acc.id } },
  });
  expect(data).toHaveLength(2);
  // Newest first, and the second upload found every row already there.
  expect(data![0].imported).toBe(0);
  expect(data![0].skipped).toBe(2);
});

test("the log is scoped to one account when asked", async ({ api, server }) => {
  const mine = await createAccount(api, "Chequing", "bank", "NZD", { institution: "ASB" });
  const other = await createAccount(api, "Savings", "savings", "NZD", { institution: "ASB" });
  await ok(
    await post(server.baseURL, asbCsv(), {
      assign: `${ASB_ACCOUNT}:${mine.id}`,
      opening_balance: false,
    })
  );

  const scoped = await api.GET("/api/imports", { params: { query: { account_id: other.id } } });
  expect(scoped.data).toEqual([]);
  const all = await api.GET("/api/imports", {});
  expect(all.data).toHaveLength(1);
});
