// Seed a running Sure backend with realistic, deterministic-ish demo data.
// Usage: BASE=http://127.0.0.1:8080 node scripts/seed.mjs
const BASE = process.env.BASE ?? "http://127.0.0.1:8080";

async function api(method, path, body) {
  const res = await fetch(`${BASE}${path}`, {
    method,
    headers: { "content-type": "application/json" },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    throw new Error(`${method} ${path} -> ${res.status} ${await res.text()}`);
  }
  return res.status === 204 ? null : res.json();
}
const post = (p, b) => api("POST", p, b);
const put = (p, b) => api("PUT", p, b);

function iso(d) {
  return d.toISOString().slice(0, 10);
}
function monthsAgo(n, day = 1) {
  const d = new Date();
  d.setMonth(d.getMonth() - n);
  d.setDate(day);
  return iso(d);
}

async function main() {
  await put("/api/settings", { base_currency_code: "NZD" });

  // Categories (nested).
  const income = (await post("/api/categories", { name: "Income", kind: "income" })).id;
  const housing = (await post("/api/categories", { name: "Housing", kind: "expense" })).id;
  const rent = (await post("/api/categories", { name: "Rent", kind: "expense", parent_id: housing })).id;
  const utilities = (await post("/api/categories", { name: "Utilities", kind: "expense", parent_id: housing })).id;
  const food = (await post("/api/categories", { name: "Food", kind: "expense" })).id;
  const groceries = (await post("/api/categories", { name: "Groceries", kind: "expense", parent_id: food })).id;
  const dining = (await post("/api/categories", { name: "Dining out", kind: "expense", parent_id: food })).id;
  const transport = (await post("/api/categories", { name: "Transport", kind: "expense" })).id;
  const lifestyle = (await post("/api/categories", { name: "Lifestyle", kind: "expense" })).id;
  const fun = (await post("/api/categories", { name: "Entertainment", kind: "expense", parent_id: lifestyle })).id;

  // Accounts.
  const everyday = (await post("/api/accounts", { name: "Everyday", kind: "bank", currency_code: "NZD" })).id;
  const savings = (await post("/api/accounts", { name: "Savings", kind: "savings", currency_code: "NZD" })).id;
  const card = (await post("/api/accounts", { name: "Credit Card", kind: "credit_card", currency_code: "NZD" })).id;
  const home = (await post("/api/accounts", { name: "Family Home", kind: "real_estate", currency_code: "NZD" })).id;
  const loan = (await post("/api/accounts", { name: "Home Loan", kind: "mortgage", currency_code: "NZD" })).id;
  const greenLoan = (await post("/api/accounts", { name: "Green Home Loan", kind: "revolving_credit", currency_code: "NZD" })).id;
  const shares = (await post("/api/accounts", { name: "Sharesies (US)", kind: "shares_us", currency_code: "USD" })).id;
  const options = (await post("/api/accounts", { name: "Startco Options", kind: "shares_private", currency_code: "USD" })).id;

  // Valuations (in minor units).
  await val(home, monthsAgo(6), 82_000_000);
  await val(loan, monthsAgo(6), -52_000_000);
  await val(greenLoan, monthsAgo(6), -3_500_000); // $35k green-energy loan

  // Link the home loans to the house as secured debt (drives the paid-off %).
  await put(`/api/accounts/${loan}/secured-by`, { secured_by_account_id: home });
  await put(`/api/accounts/${greenLoan}/secured-by`, { secured_by_account_id: home });
  await val(shares, monthsAgo(6), 1_150_000); // $11,500 USD
  await val(shares, monthsAgo(1), 1_320_000); // $13,200 USD

  // Equity: a private grant with a strike and current 409A value.
  await post(`/api/accounts/${options}/equity-grants`, {
    company: "Startco",
    grant_date: monthsAgo(28, 1),
    quantity: 12000,
    strike_minor: 120, // $1.20
    unit_value_minor: 800, // $8.00
    vest_months: 48,
    cliff_months: 12,
  });
  await post(`/api/accounts/${options}/equity/revalue`, {});

  // Custom merchants (payees), some with a default category.
  const merchNetflix = (await post("/api/merchants", { name: "Netflix", category_id: fun })).id;
  await post("/api/merchants", { name: "Countdown", category_id: groceries });
  await post("/api/merchants", { name: "New World", category_id: groceries });
  await post("/api/merchants", { name: "Z Energy", category_id: transport });

  // Transactions across the last 6 months.
  const merchants = {
    [groceries]: ["Countdown", "New World", "Pak'nSave"],
    [dining]: ["Cafe Vic", "Sushi Ten", "Thai Corner"],
    [transport]: ["Z Energy", "AT HOP", "Uber"],
    [utilities]: ["Contact Energy", "2degrees"],
    [fun]: ["Netflix", "Event Cinemas", "Spotify"],
  };
  const pick = (arr, i) => arr[i % arr.length];

  for (let m = 6; m >= 0; m--) {
    // salary
    await tx(everyday, monthsAgo(m, 2), 720_000, "Acme Payroll", income);
    // rent
    await tx(everyday, monthsAgo(m, 3), -220_000, "Property Manager", rent);
    // utilities
    await tx(everyday, monthsAgo(m, 8), -18_000 - m * 500, pick(merchants[utilities], m), utilities);
    // groceries (weekly-ish) — left uncategorised so the rule below classifies them
    for (let w = 0; w < 4; w++) {
      await tx(card, monthsAgo(m, 4 + w * 6), -(9000 + ((m + w) % 5) * 1500), pick(merchants[groceries], m + w), null);
    }
    // dining
    await tx(card, monthsAgo(m, 12), -(4500 + (m % 4) * 900), pick(merchants[dining], m), dining);
    await tx(card, monthsAgo(m, 22), -(3800 + (m % 3) * 700), pick(merchants[dining], m + 1), dining);
    // transport
    await tx(card, monthsAgo(m, 6), -(6500 + (m % 4) * 800), pick(merchants[transport], m), transport);
    // entertainment
    await tx(card, monthsAgo(m, 15), -1999, pick(merchants[fun], m), fun);
    // occasional one-off
    if (m === 3) await txOneOff(card, monthsAgo(m, 18), -145_000, "Dishwasher");
  }

  // A transfer between everyday and savings.
  await post("/api/transfers", {
    from_account_id: everyday,
    to_account_id: savings,
    posted_at: monthsAgo(1, 5),
    from_amount_minor: 100_000,
    description: "To savings",
  });

  // A rule to auto-classify supermarkets, then run it.
  const rule = await post("/api/rules", {
    name: "Supermarkets → Groceries",
    expression:
      "is_expense and (contains(lower(description), 'countdown') or contains(lower(description), 'new world') or contains(lower(description), \"pak'nsave\"))",
    set_category_id: groceries,
    overwrite_manual: false,
    stop_on_match: false,
    priority: 0,
    enabled: true,
  });
  await post(`/api/rules/${rule.id}/run`, {});

  // A rule that assigns a merchant (not just a category), then run it.
  const merchantRule = await post("/api/rules", {
    name: "Netflix → merchant",
    expression: "contains(lower(description), 'netflix')",
    set_merchant_id: merchNetflix,
    overwrite_manual: false,
    stop_on_match: false,
    priority: 1,
    enabled: true,
  });
  await post(`/api/rules/${merchantRule.id}/run`, {});

  // A cron: the house appreciates 3%/yr, applied monthly.
  const cron = await post("/api/crons", {
    name: "Home appreciation",
    account_id: home,
    kind: "appreciation",
    rate_bps: 300,
    start_date: monthsAgo(6, 1),
    enabled: true,
  });
  await post(`/api/crons/${cron.id}/run`, {});

  console.log("Seed complete.");

  async function val(account_id, as_of, value_minor) {
    await post(`/api/accounts/${account_id}/valuations`, { as_of, value_minor });
  }
  async function tx(account_id, posted_at, amount_minor, description, category_id) {
    await post("/api/transactions", { account_id, posted_at, amount_minor, description, category_id });
  }
  async function txOneOff(account_id, posted_at, amount_minor, description) {
    await post("/api/transactions", { account_id, posted_at, amount_minor, description, is_one_off: true });
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
