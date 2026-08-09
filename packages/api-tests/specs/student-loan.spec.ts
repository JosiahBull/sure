import { test, expect } from "../fixtures";
import { createAccount, createPerson, makeZip, postOversized } from "../helpers";

// Akahu reports an IR student loan's balance but no transactions, so the ledger behind the
// cutover is uploaded from myIR "TAP SLS Transactions" exports. These specs cover what the
// upload path can get wrong on its own: reading the workbook, flipping IR's sign, deduping
// a re-upload, reconciling two overlapping exports, and refusing a wrong account kind. The
// reconciliation rules themselves (coverage gaps, restatements, id numbering) are unit
// tested in `sure_providers::myir`.

// ---- a minimal .xlsx (itself a zip of XML parts) ---------------------------------------

const CONTENT_TYPES = `<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>`;
const ROOT_RELS = `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>`;
const WORKBOOK = `<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Transactions" sheetId="1" r:id="rId1"/></sheets></workbook>`;
const WORKBOOK_RELS = `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>`;

/** One `(date, transaction, amount-as-IR-signs-it)` row. */
type Row = [string, string, string];

const escape = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;");

function sheetXml(grid: string[][]): string {
  const row = (values: string[], r: number) =>
    `<row r="${r}">` +
    values
      .map(
        (v, c) =>
          `<c r="${String.fromCharCode(65 + c)}${r}" t="inlineStr"><is><t>${escape(v)}</t></is></c>`
      )
      .join("") +
    `</row>`;
  return (
    `<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>` +
    grid.map((cells, i) => row(cells, i + 1)).join("") +
    `</sheetData></worksheet>`
  );
}

/** A workbook wrapping an arbitrary grid — for the malformed-input cases. */
function xlsxFromGrid(grid: string[][]): ArrayBuffer {
  return makeZip({
    "[Content_Types].xml": CONTENT_TYPES,
    "_rels/.rels": ROOT_RELS,
    "xl/workbook.xml": WORKBOOK,
    "xl/_rels/workbook.xml.rels": WORKBOOK_RELS,
    "xl/worksheets/sheet1.xml": sheetXml(grid),
  });
}

/**
 * A well-formed myIR export: the preamble (account id + the window it is authoritative for),
 * the header row, then the transactions. Dates are written as text rather than
 * number-formatted serials — the parser accepts both, and the serial path (what a real
 * export uses) is covered by its unit tests.
 */
function xlsx(
  accountId: string,
  from: string,
  to: string,
  rows: Row[],
  holder = HOLDER
): ArrayBuffer {
  return xlsxFromGrid([
    ["Account ID:", accountId],
    // A real export names the borrower here, and with two loans in a household it is the only
    // thing in the file that says which is which — so every fixture carries it.
    ["Name:", holder],
    ["From:", from],
    ["To:", to],
    ["Period ending", "Account type", "Date", "Transaction", "Amount"],
    ...rows.map(([date, txn, amount]) => ["2026-03-31", "Student loan", date, txn, amount]),
  ]);
}

const SLS = "012-345-678-SLS004";
/** IR's `Surname, Given Names` shape with a middle initial. Invented — CLAUDE.md rule 3. */
const HOLDER = "Reed, Ari K";

/**
 * One loan's import, through the one import endpoint. `assign` names the account outright — the
 * top routing tier — which is how "the account is the path" is expressed now that there is no
 * per-account route.
 */
async function upload(baseURL: string, accountId: number, body: ArrayBuffer, dryRun = false) {
  // `source` is stated rather than sniffed, because much of this file uploads a *malformed* myIR
  // export — and a malformed file is exactly what detection cannot be trusted to place. Saying
  // which source it is gets each case to the parser whose refusal is under test, the same way the
  // UI's source picker does. Detection itself is covered by `import.spec.ts` and by
  // `sure_providers::import`'s own tests.
  const q = new URLSearchParams({
    dry_run: String(dryRun),
    source: "myir_sls",
    assign: `${SLS}:${accountId}`,
  });
  return fetch(`${baseURL}/api/import?${q}`, {
    method: "POST",
    headers: { "Content-Type": "application/zip" },
    body,
  });
}

/** The one item out of a single-loan upload — see `asb.spec.ts`'s `only` for why it's flattened. */
async function only(res: Promise<Response> | Response) {
  const body = await (await res).json();
  return { ...body, ...(body.items?.[0] ?? {}) };
}

/**
 * Two loans, no assignment, and nothing else in the file to go on: the SLS id matches no Sure
 * field, both accounts are called the same thing, and neither holds a transaction yet. The
 * `Name:` preamble is the whole answer, and this is the case the feature exists for.
 */
test("routes a myIR export to its owner's loan when the household has two", async ({
  api,
  server,
}) => {
  const ari = await createPerson(api, "Ari");
  const sam = await createPerson(api, "Sam");
  const arisLoan = await createAccount(api, "Student loan", "student_loan", "NZD", {
    ownership: { kind: "person", person_id: ari.id },
  });
  const samsLoan = await createAccount(api, "Student loan", "student_loan", "NZD", {
    ownership: { kind: "person", person_id: sam.id },
  });

  // No `assign`, so every tier has to answer for itself.
  const send = (holder: string) =>
    fetch(`${server.baseURL}/api/import?${new URLSearchParams({ source: "myir_sls" })}`, {
      method: "POST",
      headers: { "Content-Type": "application/zip" },
      body: xlsx(
        SLS,
        "2024-07-31",
        "2026-07-31",
        [["2025-04-14", "Repayment deduction", "-400.00"]],
        holder
      ),
    });

  // The household writes "Sam"; IR writes "Reed, Sam J". The name has to be found *inside*.
  const sams = await only(send("Reed, Sam J"));
  expect(sams.account_id).toBe(samsLoan.id);
  expect(sams.matched_by).toBe("account_owner");
  expect(sams.imported).toBe(1);

  // The other partner's export lands on the other loan, on the same evidence — not on the one
  // that now has history, which is the mistake this replaces.
  const aris = await only(send(HOLDER));
  expect(aris.account_id).toBe(arisLoan.id);
  expect(aris.matched_by).toBe("account_owner");
  expect(aris.imported).toBe(1);

  // A name the household doesn't answer to routes nowhere rather than to the nearest loan.
  const stranger = await only(send("Nguyen, Toni"));
  expect(stranger.account_id).toBe(null);
  expect(stranger.imported).toBe(0);
  expect(stranger.warnings.join(" ")).toContain("no account was matched");
});

/**
 * The mirror: one loan in Sure, and an export that positively names the *other* partner. "It's
 * the only one there is" stops being an answer — importing someone else's whole repayment
 * history onto your own balance reads as a successful import and is not recoverable.
 */
test("a myIR export naming someone else is not routed to the only loan there is", async ({
  api,
  server,
}) => {
  const ari = await createPerson(api, "Ari");
  await createPerson(api, "Sam");
  await createAccount(api, "Student loan", "student_loan", "NZD", {
    ownership: { kind: "person", person_id: ari.id },
  });

  const send = (holder: string) =>
    fetch(`${server.baseURL}/api/import?${new URLSearchParams({ source: "myir_sls" })}`, {
      method: "POST",
      headers: { "Content-Type": "application/zip" },
      body: xlsx(
        SLS,
        "2024-07-31",
        "2026-07-31",
        [["2025-04-14", "Repayment deduction", "-400.00"]],
        holder
      ),
    });

  const theirs = await only(send("Reed, Sam J"));
  expect(theirs.account_id).toBe(null);
  expect(theirs.imported).toBe(0);

  // …and the owner's own export still routes there, by the tier that was vetoed above.
  const mine = await only(send(HOLDER));
  expect(mine.matched_by).toBe("account_owner");
  expect(mine.imported).toBe(1);
});

test("imports a myIR export, flipping IR's sign for a liability", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");

  const res = await upload(
    server.baseURL,
    acc.id,
    xlsx(SLS, "2024-07-31", "2026-07-31", [
      ["2025-04-14", "Repayment deduction", "-400.00"],
      ["2025-04-01", "Administration fee", "40"],
      ["2025-03-10", "Living costs", "222.00"],
    ])
  );
  expect(res.status).toBe(200);
  const result = await only(res);
  expect(result.imported).toBe(3);
  expect(result.skipped).toBe(0);
  expect(result.source_account).toBe(SLS);
  expect(result.covered_from).toBe("2024-07-31");
  expect(result.covered_to).toBe("2026-07-31");
  expect(result.warnings).toEqual([]);

  const { data: txns } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  const byDescription = new Map(txns!.map((t) => [t.description, t.amount_minor]));
  // A repayment shrinks the debt, so on a liability it is positive; a fee and a drawdown
  // grow it, so they are negative. Getting this backwards inverts the net-worth line.
  expect(byDescription.get("Repayment deduction")).toBe(40000);
  expect(byDescription.get("Administration fee")).toBe(-4000);
  expect(byDescription.get("Living costs")).toBe(-22200);
});

test("re-uploading the same export imports nothing new", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const bytes = () =>
    xlsx(SLS, "2024-07-31", "2026-07-31", [["2025-04-14", "Repayment deduction", "-400.00"]]);

  expect((await only(upload(server.baseURL, acc.id, bytes()))).imported).toBe(1);

  const again = await only(upload(server.baseURL, acc.id, bytes()));
  expect(again.imported).toBe(0);
  expect(again.skipped).toBe(1);
});

test("a zip of overlapping exports reconciles into one ledger", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  // The windows overlap over 2024-01-01..2024-07-31, and both exports report the shared row
  // in it — myIR caps an export at ~2 years, so reaching origination means overlapping
  // downloads. The shared row must land once, not twice.
  const shared: Row = ["2024-03-01", "Repayment deduction", "-400.00"];
  const bundle = makeZip({
    "myir-export/older.xlsx": new Uint8Array(
      xlsx(SLS, "2022-07-31", "2024-07-31", [["2023-06-01", "Living costs", "222.00"], shared])
    ),
    "myir-export/newer.xlsx": new Uint8Array(
      xlsx(SLS, "2024-01-01", "2026-07-31", [shared, ["2025-04-14", "Repayment deduction", "-400.00"]])
    ),
  });

  const result = await only(upload(server.baseURL, acc.id, bundle));
  expect(result.imported).toBe(3);
  expect(result.covered_from).toBe("2022-07-31");
  expect(result.covered_to).toBe("2026-07-31");

  const { data: txns } = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  expect(txns).toHaveLength(3);
});

test("a gap between two uploaded exports is refused", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  // A missing window looks exactly like a quiet period once the rows are in the database,
  // so it can only be caught while both exports are in hand.
  const bundle = makeZip({
    "a.xlsx": new Uint8Array(
      xlsx(SLS, "2021-01-01", "2022-01-01", [["2021-06-01", "Living costs", "10.00"]])
    ),
    "b.xlsx": new Uint8Array(
      xlsx(SLS, "2022-01-03", "2023-01-01", [["2022-06-01", "Living costs", "10.00"]])
    ),
  });

  const res = await upload(server.baseURL, acc.id, bundle);
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("gap in coverage");
});

test("an unfamiliar transaction type is imported, with a warning", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const res = await upload(
    server.baseURL,
    acc.id,
    xlsx(SLS, "2024-07-31", "2026-07-31", [
      ["2025-04-14", "Voluntary repayment bonus", "-100.00"],
    ])
  );
  const result = await only(res);
  expect(result.imported).toBe(1);
  expect(result.warnings.join(" ")).toContain("Voluntary repayment bonus");
});

test("exports for two different loans are refused", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  // A different SLS suffix is a different product, not more history for this one.
  const bundle = makeZip({
    "mine.xlsx": new Uint8Array(
      xlsx(SLS, "2021-01-01", "2023-01-01", [["2022-06-01", "Living costs", "10.00"]])
    ),
    "theirs.xlsx": new Uint8Array(
      xlsx("012-345-678-SLS009", "2021-01-01", "2023-01-01", [
        ["2022-06-01", "Living costs", "10.00"],
      ])
    ),
  });

  const res = await upload(server.baseURL, acc.id, bundle);
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("different accounts");
});

test("uploading to a non-student-loan account is refused", async ({ api, server }) => {
  const acc = await createAccount(api, "Everyday", "bank", "NZD", { institution: "ASB" });
  const res = await upload(
    server.baseURL,
    acc.id,
    xlsx(SLS, "2024-07-31", "2026-07-31", [["2025-04-14", "Repayment deduction", "-400.00"]])
  );
  expect(res.status).toBe(422);
  // Refused on the *assignment*, before a byte is read, and it names both the file's kind and the
  // account's — which is more than "not a student loan" said.
  const message = (await res.json()).error.message;
  expect(message).toContain("can't be imported into Everyday");
  expect(message).toContain("bank");
});

// ---- malformed input --------------------------------------------------------------------
//
// Every one of these has to come back as a clean 422 naming the problem. The endpoint takes
// an arbitrary uploaded file, so "the parser panicked" or "the process ran out of memory" are
// both failures of the same kind: the request must fail, not the server.

test("a non-spreadsheet upload fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const res = await upload(server.baseURL, acc.id, new TextEncoder().encode("nope").buffer);
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("could not be understood");
});

test("an empty upload fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const res = await upload(server.baseURL, acc.id, new ArrayBuffer(0));
  expect(res.status).toBe(422);
});

test("a truncated zip fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const whole = xlsx(SLS, "2024-07-31", "2026-07-31", [
    ["2025-04-14", "Repayment deduction", "-400.00"],
  ]);
  // Half a file — the central directory the reader needs is simply not there.
  const res = await upload(server.baseURL, acc.id, whole.slice(0, Math.floor(whole.byteLength / 2)));
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("could not be understood");
});

test("a zip that is not a workbook fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  // Structurally a valid zip, but nothing calamine can open as a workbook.
  const res = await upload(server.baseURL, acc.id, makeZip({ "notes.txt": "hello" }));
  expect(res.status).toBe(422);
});

test("a workbook with no sheets fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const noSheets = makeZip({
    "[Content_Types].xml": CONTENT_TYPES,
    "_rels/.rels": ROOT_RELS,
    "xl/workbook.xml": `<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheets/></workbook>`,
    "xl/_rels/workbook.xml.rels": WORKBOOK_RELS,
  });
  const res = await upload(server.baseURL, acc.id, noSheets);
  expect(res.status).toBe(422);
});

test("a sheet with no header row fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const sheet =
    `<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>` +
    `<row r="1"><c r="A1" t="inlineStr"><is><t>Something else entirely</t></is></c></row>` +
    `</sheetData></worksheet>`;
  const res = await upload(
    server.baseURL,
    acc.id,
    makeZip({
      "[Content_Types].xml": CONTENT_TYPES,
      "_rels/.rels": ROOT_RELS,
      "xl/workbook.xml": WORKBOOK,
      "xl/_rels/workbook.xml.rels": WORKBOOK_RELS,
      "xl/worksheets/sheet1.xml": sheet,
    })
  );
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("Period ending");
});

test("a missing column fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const res = await upload(
    server.baseURL,
    acc.id,
    xlsxFromGrid([
      ["Account ID:", SLS],
      ["From:", "2024-07-31"],
      ["To:", "2026-07-31"],
      ["Period ending", "Account type", "Date", "Transaction"], // no Amount
    ])
  );
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("Amount");
});

test("a missing preamble field fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const grid = [
    ["Account ID:", SLS],
    ["From:", "2024-07-31"],
    // no To:
    ["Period ending", "Account type", "Date", "Transaction", "Amount"],
    ["2026-03-31", "Student loan", "2025-04-14", "Repayment deduction", "-400.00"],
  ];
  const res = await upload(server.baseURL, acc.id, xlsxFromGrid(grid));
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("'to'");
});

test("a non-numeric amount fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const res = await upload(
    server.baseURL,
    acc.id,
    xlsx(SLS, "2024-07-31", "2026-07-31", [["2025-04-14", "Repayment deduction", "not a number"]])
  );
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("as an amount");
});

test("an out-of-range amount fails cleanly rather than wrapping", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  // Well past i64 minor units — this must be refused, never silently truncated into a
  // plausible-looking balance.
  const res = await upload(
    server.baseURL,
    acc.id,
    xlsx(SLS, "2024-07-31", "2026-07-31", [
      ["2025-04-14", "Repayment deduction", "999999999999999999999999999"],
    ])
  );
  expect(res.status).toBe(422);
});

test("an unreadable date fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const res = await upload(
    server.baseURL,
    acc.id,
    xlsx(SLS, "2024-07-31", "2026-07-31", [["not a date", "Repayment deduction", "-400.00"]])
  );
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("as a date");
});

// ---- size ------------------------------------------------------------------------------

test("a zip bomb is refused without expanding it", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  // ~17 MB of zeros deflates to a few kilobytes. The HTTP body limit cannot see this coming;
  // only a ceiling on what the upload expands *to* stops it.
  const bomb = makeZip({ "bomb.xlsx": new Uint8Array(17 * 1024 * 1024) }, { deflate: true });
  expect(bomb.byteLength).toBeLessThan(200_000);

  const started = Date.now();
  const res = await upload(server.baseURL, acc.id, bomb);
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toMatch(/over the limit|expands/);
  // It must refuse on the declared size, not after inflating 17 MB.
  expect(Date.now() - started).toBeLessThan(5_000);
});

test("a bomb hidden inside a workbook's sheet is refused", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  // Passes as a single bare .xlsx, so the outer entry check never sees it; the sheet part is
  // what expands. calamine has no ceiling of its own, so this is caught before it gets there.
  const bomb = makeZip(
    {
      "[Content_Types].xml": CONTENT_TYPES,
      "xl/worksheets/sheet1.xml": new Uint8Array(17 * 1024 * 1024).fill(32),
    },
    { deflate: true }
  );
  expect(bomb.byteLength).toBeLessThan(200_000);

  const res = await upload(server.baseURL, acc.id, bomb);
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("decompressed");
});

test("too many workbooks in one upload are refused", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const one = xlsx(SLS, "2024-07-31", "2026-07-31", []);
  const files: Record<string, Uint8Array> = {};
  for (let i = 0; i <= 64; i++) files[`export-${i}.xlsx`] = new Uint8Array(one);

  const res = await upload(server.baseURL, acc.id, makeZip(files));
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("at most");
});

test("a body over the size limit is rejected by the server, not the parser", async ({
  api,
  server,
}) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  // The route carries the shared import body limit (50 MB). Past it the request never
  // reaches the handler at all. Probed over a raw socket rather than `fetch`: the cap is
  // enforced part-way through the upload, so the close is an RST and `undici` discards the
  // 413 it was already sent — see `postOversized`, and `http.spec.ts` for the long version.
  const res = await postOversized(server.baseURL, 51 * 1024 * 1024, {
    path: "/api/import",
    contentType: "application/zip",
  });
  expect(res.status).toBe(413);
});

test("a large but honest export imports, and stays fast", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  // One row per day for eight years. Reconciliation asks each workbook about each day, so
  // this is the shape that used to be quadratic.
  const rows: Row[] = [];
  const start = Date.UTC(2010, 0, 1);
  for (let i = 0; i < 3_000; i++) {
    const day = new Date(start + i * 86_400_000).toISOString().slice(0, 10);
    rows.push([day, "Living costs", "222.00"]);
  }

  const started = Date.now();
  const res = await upload(server.baseURL, acc.id, xlsx(SLS, "2009-01-01", "2020-01-01", rows));
  expect(res.status).toBe(200);
  const result = await only(res);
  expect(result.imported).toBe(3_000);
  expect(Date.now() - started).toBeLessThan(30_000);

  // And the whole thing still dedupes on a re-upload.
  expect((await only(upload(server.baseURL, acc.id, xlsx(SLS, "2009-01-01", "2020-01-01", rows)))).imported).toBe(0);
});

test("a one-row export imports", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const res = await upload(
    server.baseURL,
    acc.id,
    xlsx(SLS, "2024-07-31", "2026-07-31", [["2025-04-14", "Payment", "-11.11"]])
  );
  expect(res.status).toBe(200);
  expect((await only(res)).imported).toBe(1);
});

test("an export with no transactions at all is accepted as empty", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const res = await upload(server.baseURL, acc.id, xlsx(SLS, "2024-07-31", "2026-07-31", []));
  expect(res.status).toBe(200);
  const result = await only(res);
  expect(result.imported).toBe(0);
  expect(result.covered_from).toBe("2024-07-31");
});
