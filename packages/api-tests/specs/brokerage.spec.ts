import { test, expect } from "../fixtures";
import { createAccount, makeZip } from "../helpers";

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
