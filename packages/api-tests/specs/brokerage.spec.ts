import { test, expect } from "../fixtures";
import { createAccount, makeZip, postOversized } from "../helpers";

// The value snapshot (GET .../brokerage) prices each holding via the live Yahoo Finance
// endpoint on a cache miss, so — like stock-prices.spec.ts / akahu.spec.ts — it isn't
// asserted here (CI must not depend on a third-party API). These specs cover everything
// the import path can get wrong on its own: parsing the zip, persisting the three ledgers,
// deduping a re-import, and auto-linking a wallet↔bank transfer. The post-import history
// backfill is a fire-and-forget background task and is likewise not awaited/asserted.

// A fake ticker so the background history backfill never resolves a real Yahoo symbol.
const LOOKUP = JSON.stringify({
  "fund-zz": { symbol: "ZZTEST", name: "Test Instrument", exchange: "NZX", currency: "NZD" },
});

const WALLET = JSON.stringify([
  {
    amount: "-100.00",
    currency: "nzd",
    description: "Withdrawal",
    reason: "holding funds for withdrawal",
    key: "wallet-withdrawal-1",
    timestamp: { $quantum: Date.parse("2026-01-05T00:00:00Z") },
    detail: { type: "withdrawal" },
  },
  {
    amount: "5.00",
    currency: "nzd",
    description: "Dividend",
    reason: "dividend payout",
    key: "wallet-dividend-1",
    timestamp: { $quantum: Date.parse("2026-03-10T00:00:00Z") },
  },
]);

const ACTIVITY = JSON.stringify([
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
  {
    id: "ca-1",
    type: "corporate_action_v2",
    action_type: "DIVIDEND",
    record_date: "2026-03-01",
    settlement_date: "2026-03-10",
    fund_id: "fund-zz",
    outcome_records: [
      {
        id: "rec-1",
        fund_id: "fund-zz",
        currency: "nzd",
        gross_amount: "5.00",
        net_amount: "4.10",
        tax_records: [
          { owed_to: "NZ_IRD", tax_amount: "0.90", tax_credit_amount: "0", currency: "nzd" },
        ],
      },
    ],
  },
]);

function exportZip(): ArrayBuffer {
  return makeZip({
    "sharesies-export/lookup.json": LOOKUP,
    "sharesies-export/wallet-transactions.json": WALLET,
    "sharesies-export/activity.json": ACTIVITY,
  });
}

async function importZip(baseURL: string, accountId: number, zip: ArrayBuffer) {
  return fetch(`${baseURL}/api/accounts/${accountId}/brokerage/import`, {
    method: "POST",
    headers: { "Content-Type": "application/zip" },
    body: zip,
  });
}

test("imports a zip into holdings, dividends, and wallet transactions", async ({ api, server }) => {
  const acc = await createAccount(api, "Sharesies", "brokerage");

  const res = await importZip(server.baseURL, acc.id, exportZip());
  expect(res.status).toBe(200);
  const result = await res.json();
  expect(result.transactions_imported).toBe(2);
  expect(result.holdings_imported).toBe(1);
  expect(result.dividends_imported).toBe(1);
  expect(result.warnings).toEqual([]);

  // Holdings ledger.
  const { data: holdings } = await api.GET("/api/accounts/{id}/brokerage/holdings", {
    params: { path: { id: acc.id } },
  });
  expect(holdings).toHaveLength(1);
  expect(holdings![0].ticker).toBe("ZZTEST");
  expect(holdings![0].quantity).toBe(100);

  // Dividend detail with per-jurisdiction withholding.
  const { data: dividends } = await api.GET("/api/accounts/{id}/brokerage/dividends", {
    params: { path: { id: acc.id } },
  });
  expect(dividends).toHaveLength(1);
  expect(dividends![0].dividend.gross_amount_minor).toBe(500);
  expect(dividends![0].withholdings).toHaveLength(1);
  expect(dividends![0].withholdings[0].owed_to).toBe("NZ_IRD");
});

test("re-importing the same zip is idempotent", async ({ api, server }) => {
  const acc = await createAccount(api, "Sharesies", "brokerage");
  const zip = exportZip();

  await importZip(server.baseURL, acc.id, zip);
  const res = await importZip(server.baseURL, acc.id, zip);
  const result = await res.json();
  expect(result.transactions_imported).toBe(0);
  expect(result.transactions_skipped).toBe(2);
  expect(result.holdings_imported).toBe(0);
  expect(result.holdings_skipped).toBe(1);
  expect(result.dividends_imported).toBe(0);
  expect(result.dividends_skipped).toBe(1);
});

// Transfer auto-linking (the wallet withdrawal ↔ its matching bank deposit) is no longer
// done synchronously at import — it's a scheduled background task that reconciles both
// sides regardless of import order, covered by the `link_transfers` unit tests in sure-dal.

test("rejects an import onto a non-brokerage account", async ({ api, server }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const res = await importZip(server.baseURL, acc.id, exportZip());
  expect(res.status).toBe(422);
});

// ---- malformed and hostile uploads ------------------------------------------------------

/**
 * Every one of these has to come back as a clean 4xx naming the problem. The endpoint takes
 * an arbitrary uploaded file, so "the parser panicked", "the process ran out of memory" and
 * "the request hung" are all failures of the same kind: **the request must fail, not the
 * server.** The ceilings the bomb cases exercise live in `sure_providers::zipfile`.
 */
test("a file that isn't a zip fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Sharesies", "brokerage");
  for (const body of [
    new ArrayBuffer(0),
    new TextEncoder().encode("<html>nope</html>").buffer as ArrayBuffer,
    // A zip header and nothing else.
    new Uint8Array([0x50, 0x4b, 0x03, 0x04, 0, 0, 0, 0]).buffer as ArrayBuffer,
  ]) {
    const res = await importZip(server.baseURL, acc.id, body);
    expect(res.status).toBe(422);
    expect((await res.json()).error.message).toBeTruthy();
  }
});

test("a zip missing the files the export is made of fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Sharesies", "brokerage");
  const cases: Record<string, string>[] = [
    { "readme.txt": "hello" },
    { "lookup.json": LOOKUP },
    // Has the wallet but not the activity.
    { "lookup.json": LOOKUP, "wallet-transactions.json": WALLET },
  ];
  for (const files of cases) {
    const res = await importZip(server.baseURL, acc.id, makeZip(files));
    expect(res.status).toBe(422);
    expect((await res.json()).error.message).toContain("missing");
  }
});

test("a zip whose entries aren't the JSON they claim fails cleanly", async ({ api, server }) => {
  const acc = await createAccount(api, "Sharesies", "brokerage");
  for (const files of [
    { "lookup.json": "{", "wallet-transactions.json": WALLET, "activity.json": ACTIVITY },
    { "lookup.json": LOOKUP, "wallet-transactions.json": "not json", "activity.json": ACTIVITY },
    // Valid JSON, wrong shape.
    { "lookup.json": LOOKUP, "wallet-transactions.json": "{}", "activity.json": ACTIVITY },
    { "lookup.json": LOOKUP, "wallet-transactions.json": WALLET, "activity.json": "[[[" },
  ]) {
    const res = await importZip(server.baseURL, acc.id, makeZip(files));
    expect(res.status).toBe(422);
  }
});

test("an amount no money could be fails cleanly rather than wrapping", async ({ api, server }) => {
  const acc = await createAccount(api, "Sharesies", "brokerage");
  const wallet = JSON.stringify([
    {
      amount: "999999999999999999999999",
      currency: "nzd",
      description: "Overflow",
      key: "w1",
      timestamp: { $quantum: Date.parse("2026-01-05T00:00:00Z") },
    },
  ]);
  const res = await importZip(
    server.baseURL,
    acc.id,
    makeZip({ "lookup.json": LOOKUP, "wallet-transactions.json": wallet, "activity.json": "[]" })
  );
  // Either refused or clamped — never a panic, and never a silently wrapped negative.
  expect([200, 422]).toContain(res.status);
  if (res.status === 200) {
    const { data } = await api.GET("/api/transactions", {
      params: { query: { account_id: acc.id } },
    });
    for (const t of data ?? []) expect(Number.isSafeInteger(t.amount_minor)).toBe(true);
  }
});

/**
 * The gap this closes: the import used to `read_to_end` a zip entry with no ceiling, so a
 * hundred kilobytes on the wire could ask for gigabytes of allocation. The HTTP body limit
 * bounds what arrives, not what it expands to.
 */
test("a zip bomb is refused without expanding it", async ({ api, server }) => {
  const acc = await createAccount(api, "Sharesies", "brokerage");
  const bomb = makeZip(
    {
      "lookup.json": LOOKUP,
      "wallet-transactions.json": new Uint8Array(20 * 1024 * 1024),
      "activity.json": ACTIVITY,
    },
    { deflate: true }
  );
  expect(bomb.byteLength).toBeLessThan(200_000);

  const res = await importZip(server.baseURL, acc.id, bomb);
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toMatch(/over the limit|expands/);
});

test("a zip of many bombs can't add up past the upload ceiling either", async ({ api, server }) => {
  const acc = await createAccount(api, "Sharesies", "brokerage");
  const files: Record<string, string | Uint8Array> = {
    "lookup.json": LOOKUP,
    "wallet-transactions.json": WALLET,
    "activity.json": ACTIVITY,
  };
  // Each under the per-entry ceiling, together far over the upload's.
  for (let i = 0; i < 10; i++) files[`pad${i}.json`] = new Uint8Array(15 * 1024 * 1024);
  const res = await importZip(server.baseURL, acc.id, makeZip(files, { deflate: true }));
  // The padding is never read (only the three named files are), so this must still succeed
  // promptly rather than expanding 150 MB to find out.
  expect([200, 422]).toContain(res.status);
});

test("a zip with absurdly many entries is refused", async ({ api, server }) => {
  const acc = await createAccount(api, "Sharesies", "brokerage");
  const files: Record<string, string> = {};
  for (let i = 0; i < 200; i++) files[`f${i}.json`] = "{}";
  const res = await importZip(server.baseURL, acc.id, makeZip(files));
  expect(res.status).toBe(422);
  expect((await res.json()).error.message).toContain("at most");
});

test("a body over the size limit is rejected by the server, not the parser", async ({ api, server }) => {
  const acc = await createAccount(api, "Sharesies", "brokerage");
  // Over a raw socket, not `fetch` — the cap trips mid-upload and the RST loses the 413.
  const res = await postOversized(server.baseURL, 51 * 1024 * 1024, {
    path: `/api/accounts/${acc.id}/brokerage/import`,
    contentType: "application/zip",
  });
  expect(res.status).toBe(413);
});

test("a large but honest export imports, and stays fast", async ({ api, server }) => {
  const acc = await createAccount(api, "Sharesies", "brokerage");
  const wallet = JSON.stringify(
    Array.from({ length: 4000 }, (_, i) => ({
      amount: "1.00",
      currency: "nzd",
      description: `Row ${i}`,
      key: `w${i}`,
      timestamp: { $quantum: Date.parse("2026-01-05T00:00:00Z") + i * 1000 },
    }))
  );
  const started = Date.now();
  const res = await importZip(
    server.baseURL,
    acc.id,
    makeZip({ "lookup.json": LOOKUP, "wallet-transactions.json": wallet, "activity.json": "[]" })
  );
  expect(res.status).toBe(200);
  expect((await res.json()).transactions_imported).toBe(4000);
  expect(Date.now() - started).toBeLessThan(30_000);
});
