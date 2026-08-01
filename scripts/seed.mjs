// Seed a running Sure backend with realistic, deterministic-ish demo data.
// Usage: BASE=http://127.0.0.1:8080 node scripts/seed.mjs
const BASE = process.env.BASE ?? "http://127.0.0.1:8080";

// Every date below is derived from this one instant, so `SEED_TODAY=YYYY-MM-DD` makes the
// whole data set reproducible — which is what lets the web visual suite pin it and get
// byte-identical screenshots on any day (see packages/web/tests/demo-date.ts). Unset, it
// means now, so a normal `pnpm seed` still produces data that looks current.
// Normalised to midday UTC either way, so the calendar date can't shift under a ±13h local
// offset: an explicit SEED_TODAY is read as that date, and an absent one means whatever
// date it is *locally*, not in UTC (those differ for half of every day in NZ).
const TODAY = process.env.SEED_TODAY
  ? new Date(`${process.env.SEED_TODAY}T12:00:00Z`)
  : (() => {
      const local = new Date();
      return new Date(Date.UTC(local.getFullYear(), local.getMonth(), local.getDate(), 12));
    })();
if (Number.isNaN(TODAY.getTime())) {
  throw new Error(`SEED_TODAY is not a YYYY-MM-DD date: ${process.env.SEED_TODAY}`);
}

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
// Built from calendar components rather than `setMonth` then `setDate`: stepping the month
// first off a 29th–31st overflows into the following month (June 31 → July 1), which
// silently moved a month's data around depending on what day the seed happened to run.
function monthsAgo(n, day = 1) {
  return iso(new Date(Date.UTC(TODAY.getUTCFullYear(), TODAY.getUTCMonth() - n, day, 12)));
}
function monthsAhead(n, day = 1) {
  return monthsAgo(-n, day);
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
  const transfers = (await post("/api/categories", { name: "Transfers", kind: "transfer" })).id;
  const bankFees = (await post("/api/categories", { name: "Bank fees", kind: "expense" })).id;

  // The household. Every account has to name an owner, so a database always starts with a
  // placeholder person (see migration 0016) — renaming it is what the app asks a real user to
  // do, and it's what the demo does here rather than leaving a stand-in lying around.
  const existingPeople = await api("GET", "/api/people");
  const placeholder = existingPeople.find((p) => p.placeholder);
  const ari = placeholder
    ? (await put(`/api/people/${placeholder.id}`, { name: "Ari", color: "#7c5cff", sort_order: 0 })).id
    : (await post("/api/people", { name: "Ari", color: "#7c5cff", sort_order: 0 })).id;
  const sam = (await post("/api/people", { name: "Sam", color: "#12b981", sort_order: 1 })).id;
  // Shorthands for the three answers an account can give.
  const owns = (personId) => ({ ownership: { kind: "person", person_id: personId } });
  const joint = { ownership: { kind: "joint" } };

  // Accounts, each with typed, per-kind metadata (see AccountMetadata in the backend).
  //
  // Every field the backend now requires per kind is spelled out here (a property's
  // subtype/city/country, a loan's principal, a card's limit, ...), so this file doubles as a
  // worked example of a complete account. Each create also carries an *opening* balance of
  // zero: these accounts get their history from the valuations and transactions seeded below,
  // and zero deliberately seeds no ledger row, so the demo numbers stay exactly as they read.
  const openingBalance = { opening_balance_minor: 0, opening_balance_date: monthsAgo(38, 15) };
  const everyday = (await post("/api/accounts", {
    name: "Everyday", kind: "bank", currency_code: "NZD", institution: "ANZ", ...openingBalance, ...owns(ari),
    metadata: { profile: "depository", subtype: "checking", account_number: "••4821", url: "https://www.anz.co.nz" },
  })).id;
  const savings = (await post("/api/accounts", {
    name: "Savings", kind: "savings", currency_code: "NZD", institution: "ANZ", ...openingBalance, ...joint,
    metadata: { profile: "depository", subtype: "savings", account_number: "••5502" },
  })).id;
  const card = (await post("/api/accounts", {
    name: "Credit Card", kind: "credit_card", currency_code: "NZD", institution: "American Express",
    ...openingBalance, ...owns(ari),
    metadata: {
      profile: "depository",
      account_number: "••1009",
      credit_limit_minor: 1_000_000, // $10,000
    },
  })).id;
  const home = (await post("/api/accounts", {
    name: "Family Home", kind: "real_estate", currency_code: "NZD", ...openingBalance, ...joint,
    metadata: {
      profile: "property",
      subtype: "single_family_home",
      address_line1: "14 Kōwhai Street",
      city: "Wellington",
      country: "New Zealand",
      purchase_date: monthsAgo(38, 15),
      purchase_price_minor: 74_000_000, // $740,000
      url: "https://www.qv.co.nz",
    },
  })).id;
  const loan = (await post("/api/accounts", {
    name: "Home Loan", kind: "mortgage", currency_code: "NZD", ...openingBalance, ...joint,
    metadata: {
      profile: "mortgage",
      lender: "ANZ",
      original_amount_minor: 58_500_000, // $585,000 — $520,000 still owed plus $65,000 repaid
      interest_rate_bps: 649, // 6.49%
      rate_type: "fixed",
      fixed_until: monthsAhead(14, 1),
      fixed_term_months: 24,
      // What to assume once that fix expires, and how unsure of it we are — this is what
      // gives the forecast an honest band around the mortgage instead of one flat line.
      refix_rate_bps: 649,
      refix_rate_uncertainty_bps: 150, // ±1.5%, one standard deviation
      term_months: 360,
      start_date: monthsAgo(38, 15),
      repayment_minor: 169_000, // $1,690/fortnight
      repayment_frequency: "fortnightly",
      interest_paid_minor: 9_800_000, // $98,000
      capital_paid_minor: 6_500_000, // $65,000
    },
  })).id;
  const greenLoan = (await post("/api/accounts", {
    name: "Green Home Loan", kind: "revolving_credit", currency_code: "NZD", institution: "ANZ",
    ...openingBalance, ...joint,
    metadata: {
      profile: "depository",
      credit_limit_minor: 3_500_000, // $35,000, fully drawn
      notes: "Interest-free green-energy top-up (solar + insulation).",
    },
  })).id;
  const shares = (await post("/api/accounts", {
    name: "Sharesies (US)", kind: "shares_us", currency_code: "USD", ...openingBalance, ...owns(sam),
    metadata: { profile: "shares", broker: "Sharesies", ticker: "VOO", exchange: "NYSE Arca" },
  })).id;
  const options = (await post("/api/accounts", {
    name: "Startco Options", kind: "shares_private", currency_code: "USD", ...openingBalance, ...owns(sam),
    metadata: { profile: "shares", broker: "Carta", ticker: "STARTCO" },
  })).id;
  const car = (await post("/api/accounts", {
    name: "Family Car", kind: "vehicle", currency_code: "NZD", ...openingBalance, ...joint,
    metadata: {
      profile: "vehicle",
      make: "Toyota", model: "RAV4", year: 2021, plate: "MEP123", nickname: "the wagon",
      purchase_date: monthsAgo(20, 10),
    },
  })).id;
  const carLoan = (await post("/api/accounts", {
    name: "Car Loan", kind: "loan", currency_code: "NZD", ...openingBalance, ...joint,
    metadata: {
      profile: "loan",
      subtype: "auto",
      lender: "MTF Finance",
      original_amount_minor: 2_500_000, // $25,000
      interest_rate_bps: 890, // 8.90%
      // Fixed for the whole term, as car finance usually is, so there's no refix to assume.
      rate_type: "floating",
      term_months: 60,
      start_date: monthsAgo(20, 10),
    },
  })).id;

  // Valuations (in minor units).
  await val(home, monthsAgo(6), 82_000_000);
  await val(loan, monthsAgo(6), -52_000_000);
  await val(greenLoan, monthsAgo(6), -3_500_000); // $35k green-energy loan
  await val(car, monthsAgo(20), 4_200_000); // bought at $42,000
  await val(car, monthsAgo(1), 3_450_000); // now ~$34,500
  await val(carLoan, monthsAgo(6), -1_500_000); // owe $15,000

  // Link the home loans to the house, and the car loan to the car (drives paid-off %).
  await put(`/api/accounts/${loan}/secured-by`, { secured_by_account_id: home });
  await put(`/api/accounts/${greenLoan}/secured-by`, { secured_by_account_id: home });
  await put(`/api/accounts/${carLoan}/secured-by`, { secured_by_account_id: car });
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

  // Bank-statement-style entries that don't arrive pre-categorised from a feed —
  // left uncategorised so the rules below classify them.
  await tx(everyday, monthsAgo(2, 20), -50_000, "MB TRANSFER TO 12-3456-0000000-00", null);
  await tx(savings, monthsAgo(2, 20), 50_000, "MB TRANSFER FROM 12-3456-0000000-00", null);
  await tx(savings, monthsAgo(1, 1), 42, "CR.INT TO 01/06/2026", null);
  await tx(card, monthsAgo(2, 9), -600, "ORIKAN NEW ZEALAND LTDALBANY CARD 1234", null);
  await tx(card, monthsAgo(1, 14), -1950, "Twinkl 10000000Sheffield CARD 1234", null);
  await tx(card, monthsAgo(1, 12), -57, "OffshoreServiceMargins", null);

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

  // A rule recognising internal bank transfers by their statement wording, then run it.
  const transferRule = await post("/api/rules", {
    name: "Internal bank transfers → Transfers",
    expression: "contains(lower(description), 'mb transfer')",
    set_category_id: transfers,
    overwrite_manual: false,
    stop_on_match: false,
    priority: 2,
    enabled: true,
  });
  await post(`/api/rules/${transferRule.id}/run`, {});

  // A rule recognising bank-paid interest, then run it.
  const interestRule = await post("/api/rules", {
    name: "Bank interest received → Income",
    expression: "contains(lower(description), 'cr.int')",
    set_category_id: income,
    overwrite_manual: false,
    stop_on_match: false,
    priority: 3,
    enabled: true,
  });
  await post(`/api/rules/${interestRule.id}/run`, {});

  // A rule recognising a named merchant not worth a full custom-merchant entry, then run it.
  const parkingRule = await post("/api/rules", {
    name: "Orikan → Transport",
    expression: "contains(lower(description), 'orikan')",
    set_category_id: transport,
    overwrite_manual: false,
    stop_on_match: false,
    priority: 4,
    enabled: true,
  });
  await post(`/api/rules/${parkingRule.id}/run`, {});

  const subscriptionRule = await post("/api/rules", {
    name: "Twinkl → Entertainment",
    expression: "contains(lower(description), 'twinkl')",
    set_category_id: fun,
    overwrite_manual: false,
    stop_on_match: false,
    priority: 5,
    enabled: true,
  });
  await post(`/api/rules/${subscriptionRule.id}/run`, {});

  // A rule recognising foreign-transaction fee line items, then run it.
  const fxFeeRule = await post("/api/rules", {
    name: "Offshore FX fee → Bank fees",
    expression: "contains(lower(description), 'offshoreservicemargins')",
    set_category_id: bankFees,
    overwrite_manual: false,
    stop_on_match: false,
    priority: 6,
    enabled: true,
  });
  await post(`/api/rules/${fxFeeRule.id}/run`, {});

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

  // The car depreciates 12%/yr, applied monthly.
  const carCron = await post("/api/crons", {
    name: "Car depreciation",
    account_id: car,
    kind: "depreciation",
    rate_bps: 1200,
    start_date: monthsAgo(6, 1),
    enabled: true,
  });
  await post(`/api/crons/${carCron.id}/run`, {});

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
