import { test, expect } from "../fixtures";
import { createAccount, makeZip } from "../helpers";

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

/**
 * A myIR export: the preamble (account id + the window it is authoritative for), the header
 * row, then the transactions. Dates are written as text rather than number-formatted
 * serials — the parser accepts both, and the serial path is covered by its unit tests.
 */
function exportXlsx(accountId: string, from: string, to: string, rows: Row[]): string {
  const cells = (values: string[], r: number) =>
    `<row r="${r}">` +
    values
      .map(
        (v, c) =>
          `<c r="${String.fromCharCode(65 + c)}${r}" t="inlineStr"><is><t>${v}</t></is></c>`
      )
      .join("") +
    `</row>`;

  const grid: string[][] = [
    ["Account ID:", accountId],
    ["From:", from],
    ["To:", to],
    ["Period ending", "Account type", "Date", "Transaction", "Amount"],
    ...rows.map(([date, txn, amount]) => ["2026-03-31", "Student loan", date, txn, amount]),
  ];

  return (
    `<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>` +
    grid.map((row, i) => cells(row, i + 1)).join("") +
    `</sheetData></worksheet>`
  );
}

function xlsx(accountId: string, from: string, to: string, rows: Row[]): ArrayBuffer {
  return makeZip({
    "[Content_Types].xml": CONTENT_TYPES,
    "_rels/.rels": ROOT_RELS,
    "xl/workbook.xml": WORKBOOK,
    "xl/_rels/workbook.xml.rels": WORKBOOK_RELS,
    "xl/worksheets/sheet1.xml": exportXlsx(accountId, from, to, rows),
  });
}

const SLS = "012-345-678-SLS004";

async function upload(baseURL: string, accountId: number, body: ArrayBuffer) {
  return fetch(`${baseURL}/api/accounts/${accountId}/student-loan/import`, {
    method: "POST",
    headers: { "Content-Type": "application/zip" },
    body,
  });
}

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
  const result = await res.json();
  expect(result.imported).toBe(3);
  expect(result.skipped).toBe(0);
  expect(result.account_id).toBe(SLS);
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

  expect((await (await upload(server.baseURL, acc.id, bytes())).json()).imported).toBe(1);

  const again = await (await upload(server.baseURL, acc.id, bytes())).json();
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

  const result = await (await upload(server.baseURL, acc.id, bundle)).json();
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
  const result = await res.json();
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
  expect((await res.json()).error.message).toContain("not a student loan");
});

test("a non-spreadsheet upload fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Student loan", "student_loan");
  const res = await upload(server.baseURL, acc.id, new TextEncoder().encode("nope").buffer);
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("could not read export");
});
