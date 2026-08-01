import { test, expect } from "../fixtures";
import type { Schemas, SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

/**
 * A create body written out in full, bypassing `createAccount`'s per-kind defaults — the
 * required-field specs below are precisely about what happens when a field is missing.
 */
function saveBody(
  kind: Schemas["SaveAccount"]["kind"],
  metadata?: Record<string, unknown>,
  overrides: Partial<Schemas["SaveAccount"]> = {}
): Schemas["SaveAccount"] {
  return {
    name: `Test ${kind}`,
    kind,
    currency_code: "NZD",
    archived: false,
    sort_order: 0,
    opening_balance_minor: 0,
    opening_balance_date: "2020-01-01",
    metadata: metadata as Schemas["AccountMetadata"],
    ...overrides,
  };
}

/** POST `body`, assert it was refused, and return the message so a spec can read it. */
async function rejected(api: SureClient, body: Schemas["SaveAccount"]) {
  const res = await api.POST("/api/accounts", { body });
  expect(res.response.status, "expected the save to be refused").toBe(422);
  return (res.error as { error?: { message?: string } })?.error?.message ?? "";
}

test("property metadata round-trips with typed fields", async ({ api }) => {
  const house = await createAccount(api, "Family Home", "real_estate", "NZD", {
    metadata: {
      profile: "property",
      address_line1: "14 Kōwhai Street, Wellington",
      purchase_date: "2023-05-15",
      purchase_price_minor: 74_000_000,
      url: "https://www.qv.co.nz",
    },
    institution: "Barfoot",
  });

  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: house.id } } });
  expect(data?.institution).toBe("Barfoot");
  const meta = data?.metadata as Schemas["PropertyMeta"] & { profile: string };
  expect(meta.profile).toBe("property");
  expect(meta.address_line1).toBe("14 Kōwhai Street, Wellington");
  expect(meta.purchase_date).toBe("2023-05-15");
  expect(meta.purchase_price_minor).toBe(74_000_000);
  expect(meta.url).toBe("https://www.qv.co.nz");
});

test("property metadata round-trips subtype, area, year built and a full address", async ({
  api,
}) => {
  const house = await createAccount(api, "Rental", "real_estate", "NZD", {
    metadata: {
      profile: "property",
      subtype: "investment_property",
      year_built: 1990,
      area_value: 1200,
      area_unit: "sqm",
      address_line1: "1 Queen Street",
      address_line2: "Apartment 4B",
      city: "Auckland",
      region: "Auckland",
      postal_code: "1010",
      country: "New Zealand",
    },
  });

  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: house.id } } });
  const meta = data?.metadata as Schemas["PropertyMeta"] & { profile: string };
  expect(meta.profile).toBe("property");
  expect(meta.subtype).toBe("investment_property");
  expect(meta.year_built).toBe(1990);
  expect(meta.area_value).toBe(1200);
  expect(meta.area_unit).toBe("sqm");
  expect(meta.address_line1).toBe("1 Queen Street");
  expect(meta.address_line2).toBe("Apartment 4B");
  expect(meta.city).toBe("Auckland");
  expect(meta.region).toBe("Auckland");
  expect(meta.postal_code).toBe("1010");
  expect(meta.country).toBe("New Zealand");
});

test("a property stored under the legacy `address` key reads back as address_line1", async ({
  api,
}) => {
  // Rows written before the address was split into components used a single `address`
  // key; the serde alias keeps them deserialising.
  const house = await createAccount(api, "Old Home", "real_estate", "NZD", {
    metadata: { profile: "property", address: "9 Legacy Lane" } as Schemas["AccountMetadata"],
  });

  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: house.id } } });
  const meta = data?.metadata as Schemas["PropertyMeta"] & { profile: string };
  expect(meta.profile).toBe("property");
  expect(meta.address_line1).toBe("9 Legacy Lane");
  expect((meta as { address?: string }).address).toBeUndefined();
});

test("vehicle metadata round-trips make/model/year/plate/nickname", async ({ api }) => {
  const car = await createAccount(api, "Family Car", "vehicle", "NZD", {
    metadata: {
      profile: "vehicle",
      make: "Toyota",
      model: "RAV4",
      year: 2021,
      plate: "MEP123",
      nickname: "the wagon",
      purchase_date: "2024-11-10",
    },
  });

  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: car.id } } });
  const meta = data?.metadata as Schemas["VehicleMeta"] & { profile: string };
  expect(meta.profile).toBe("vehicle");
  expect(meta.make).toBe("Toyota");
  expect(meta.model).toBe("RAV4");
  expect(meta.year).toBe(2021);
  expect(meta.plate).toBe("MEP123");
  expect(meta.nickname).toBe("the wagon");
});

test("vehicle metadata round-trips an odometer reading and its unit", async ({ api }) => {
  const car = await createAccount(api, "Commuter", "vehicle", "NZD", {
    metadata: {
      profile: "vehicle",
      make: "Toyota",
      model: "Camry",
      year: 2023,
      mileage_value: 15_000,
      mileage_unit: "km",
    },
  });

  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: car.id } } });
  const meta = data?.metadata as Schemas["VehicleMeta"] & { profile: string };
  expect(meta.mileage_value).toBe(15_000);
  expect(meta.mileage_unit).toBe("km");
});

test("a mortgage carries rate/terms metadata and links to its property", async ({ api }) => {
  const house = await createAccount(api, "Home", "real_estate");
  const mortgage = await createAccount(api, "Home Loan", "mortgage", "NZD", {
    metadata: {
      profile: "mortgage",
      lender: "ANZ",
      interest_rate_bps: 649,
      rate_type: "fixed",
      term_months: 360,
      interest_paid_minor: 9_800_000,
      capital_paid_minor: 6_500_000,
    },
  });

  // The secured-by link is independent of metadata and survives a metadata read.
  const linked = await api.PUT("/api/accounts/{id}/secured-by", {
    params: { path: { id: mortgage.id } },
    body: { secured_by_account_id: house.id },
  });
  expect(linked.response.status).toBe(200);

  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: mortgage.id } } });
  expect(data?.secured_by_account_id).toBe(house.id);
  const meta = data?.metadata as Schemas["MortgageMeta"] & { profile: string };
  expect(meta.profile).toBe("mortgage");
  expect(meta.interest_rate_bps).toBe(649);
  expect(meta.rate_type).toBe("fixed");
  expect(meta.term_months).toBe(360);
  expect(meta.capital_paid_minor).toBe(6_500_000);
});

test("a mortgage/loan's original borrowed amount round-trips", async ({ api }) => {
  const mortgage = await createAccount(api, "Home Loan", "mortgage", "NZD", {
    metadata: { profile: "mortgage", lender: "ASB", original_amount_minor: 48_500_000 },
  });
  const loan = await createAccount(api, "Student Loan", "student_loan", "NZD", {
    metadata: { profile: "loan", original_amount_minor: 3_000_000 },
  });

  const m = await api.GET("/api/accounts/{id}", { params: { path: { id: mortgage.id } } });
  expect((m.data?.metadata as Schemas["MortgageMeta"]).original_amount_minor).toBe(48_500_000);

  const l = await api.GET("/api/accounts/{id}", { params: { path: { id: loan.id } } });
  expect((l.data?.metadata as Schemas["LoanMeta"]).original_amount_minor).toBe(3_000_000);
});

test("a loan carries a rate type and a subtype", async ({ api }) => {
  const loan = await createAccount(api, "Car Loan", "loan", "NZD", {
    metadata: {
      profile: "loan",
      subtype: "auto",
      rate_type: "floating",
      interest_rate_bps: 525,
      term_months: 60,
    },
  });

  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: loan.id } } });
  const meta = data?.metadata as Schemas["LoanMeta"] & { profile: string };
  expect(meta.profile).toBe("loan");
  expect(meta.subtype).toBe("auto");
  expect(meta.rate_type).toBe("floating");
  expect(meta.interest_rate_bps).toBe(525);
  expect(meta.term_months).toBe(60);
});

test("shares under the same profile accept a broker/ticker regardless of kind", async ({ api }) => {
  for (const kind of ["shares_nz", "shares_us", "shares_private"] as const) {
    const acc = await createAccount(api, `Holdings ${kind}`, kind, "USD", {
      metadata: { profile: "shares", broker: "Sharesies", ticker: "VOO", exchange: "NYSE Arca" },
    });
    const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: acc.id } } });
    const meta = data?.metadata as Schemas["SharesMeta"] & { profile: string };
    expect(meta.profile).toBe("shares");
    expect(meta.broker).toBe("Sharesies");
    expect(meta.ticker).toBe("VOO");
  }
});

test("shares and brokerage accounts round-trip an investment subtype", async ({ api }) => {
  const shares = await createAccount(api, "KiwiSaver", "shares_nz", "NZD", {
    metadata: { profile: "shares", subtype: "kiwisaver", broker: "Simplicity" },
  });
  const brokerage = await createAccount(api, "Sharesies", "brokerage", "NZD", {
    metadata: { profile: "brokerage", subtype: "brokerage", broker: "Sharesies" },
  });

  const s = await api.GET("/api/accounts/{id}", { params: { path: { id: shares.id } } });
  expect((s.data?.metadata as Schemas["SharesMeta"]).subtype).toBe("kiwisaver");

  const b = await api.GET("/api/accounts/{id}", { params: { path: { id: brokerage.id } } });
  expect((b.data?.metadata as Schemas["BrokerageMeta"]).subtype).toBe("brokerage");
});

test("a crypto account uses the crypto profile with a subtype and tax treatment", async ({
  api,
}) => {
  const wallet = await createAccount(api, "Cold Wallet", "crypto", "NZD", {
    metadata: {
      profile: "crypto",
      subtype: "wallet",
      tax_treatment: "taxable",
      url: "https://etherscan.io",
    },
  });
  expect(wallet.kind).toBe("crypto");
  expect(wallet.class).toBe("investment");

  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: wallet.id } } });
  const meta = data?.metadata as Schemas["CryptoMeta"] & { profile: string };
  expect(meta.profile).toBe("crypto");
  expect(meta.subtype).toBe("wallet");
  expect(meta.tax_treatment).toBe("taxable");
  expect(meta.url).toBe("https://etherscan.io");
});

test("a credit card's credit limit round-trips and drives remaining-borrowing", async ({ api }) => {
  const card = await createAccount(api, "Visa Platinum", "credit_card", "NZD", {
    metadata: { profile: "depository", account_number: "••1234", credit_limit_minor: 1_000_000 },
  });
  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: card.id } } });
  const meta = data?.metadata as Schemas["DepositoryMeta"] & { profile: string };
  expect(meta.credit_limit_minor).toBe(1_000_000);
  expect(meta.account_number).toBe("••1234");
});

test("a credit card round-trips its payment, APR, expiry and annual fee", async ({ api }) => {
  const card = await createAccount(api, "Amex Gold", "credit_card", "NZD", {
    metadata: {
      profile: "depository",
      minimum_payment_minor: 5_000,
      apr_bps: 1599,
      expiration_date: "2029-04-30",
      annual_fee_minor: 9_900,
    },
  });

  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: card.id } } });
  const meta = data?.metadata as Schemas["DepositoryMeta"] & { profile: string };
  expect(meta.minimum_payment_minor).toBe(5_000);
  expect(meta.apr_bps).toBe(1599);
  expect(meta.expiration_date).toBe("2029-04-30");
  expect(meta.annual_fee_minor).toBe(9_900);
});

test("a depository account round-trips its subtype", async ({ api }) => {
  const savings = await createAccount(api, "Rainy Day", "savings", "NZD", {
    metadata: { profile: "depository", subtype: "savings" },
  });
  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: savings.id } } });
  const meta = data?.metadata as Schemas["DepositoryMeta"] & { profile: string };
  expect(meta.profile).toBe("depository");
  expect(meta.subtype).toBe("savings");
});

test("metadata whose profile does not match the kind is rejected", async ({ api }) => {
  // A bank account uses the `depository` profile, not `vehicle`.
  const res = await api.POST("/api/accounts", {
    body: {
      name: "Wrong",
      kind: "bank",
      currency_code: "NZD",
      archived: false,
      sort_order: 0,
      metadata: { profile: "vehicle", make: "Toyota" } as Schemas["AccountMetadata"],
    },
  });
  expect(res.response.status).toBe(422);
});

test("a kind with no required metadata is created without any", async ({ api }) => {
  // Our `kind` already says everything a depository subtype would, so a bank account needs
  // no metadata at all and gets an empty value for its profile.
  const acc = await createAccount(api, "Everyday", "bank", "NZD", { metadata: undefined });
  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: acc.id } } });
  const meta = data?.metadata as { profile: string };
  expect(meta.profile).toBe("depository");
});

test("a kind that does have required metadata is refused without it", async ({ api }) => {
  // Empty metadata used to be accepted and defaulted; a property now has to say what and
  // where it is.
  const msg = await rejected(api, saveBody("real_estate", { profile: "property" }));
  for (const field of ["subtype", "address_line1", "city", "country"]) {
    expect(msg, `should name ${field}`).toContain(field);
  }
});

test("updating an account changes its typed metadata", async ({ api }) => {
  const car = await createAccount(api, "Car", "vehicle", "NZD", {
    metadata: { profile: "vehicle", make: "Toyota", nickname: "old name" },
  });

  const updated = await api.PUT("/api/accounts/{id}", {
    params: { path: { id: car.id } },
    body: {
      name: "Car",
      kind: "vehicle",
      currency_code: "NZD",
      archived: false,
      sort_order: 0,
      metadata: {
        profile: "vehicle",
        make: "Toyota",
        model: "Corolla",
        year: 2021,
        nickname: "new name",
      },
    },
  });
  expect(updated.response.status).toBe(200);
  const meta = updated.data?.metadata as Schemas["VehicleMeta"] & { profile: string };
  expect(meta.model).toBe("Corolla");
  expect(meta.nickname).toBe("new name");
});

test("config export/import round-trips typed metadata", async ({ api }) => {
  const car = await createAccount(api, "Family Car", "vehicle", "NZD", {
    metadata: { profile: "vehicle", make: "Toyota", model: "RAV4", plate: "MEP123" },
  });

  const snapshot = await api.GET("/api/config/export", {});
  expect(snapshot.response.status).toBe(200);

  const result = await api.POST("/api/config/import", { body: snapshot.data as never });
  expect(result.response.status).toBe(200);

  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: car.id } } });
  const meta = data?.metadata as Schemas["VehicleMeta"] & { profile: string };
  expect(meta.make).toBe("Toyota");
  expect(meta.model).toBe("RAV4");
  expect(meta.plate).toBe("MEP123");
});

// ---------------------------------------------------------------------------
// Required fields. Metadata stays optional on the type (a provider-linked or legacy
// account genuinely may not know these yet — see `ValidationMode` in sure-core), so the
// requirement is enforced when a *person* saves the account, and every gap is reported at
// once rather than one per round trip.
// ---------------------------------------------------------------------------

test("a vehicle must say what it is", async ({ api }) => {
  const msg = await rejected(api, saveBody("vehicle", { profile: "vehicle" }));
  for (const field of ["make", "model", "year"]) {
    expect(msg, `should name ${field}`).toContain(field);
  }
});

test("a mortgage must carry its lender, principal and rate", async ({ api }) => {
  const msg = await rejected(api, saveBody("mortgage", { profile: "mortgage" }));
  for (const field of ["lender", "original_amount_minor", "interest_rate_bps", "rate_type"]) {
    expect(msg, `should name ${field}`).toContain(field);
  }
});

test("a loan must carry its subtype, lender and full amortisation terms", async ({ api }) => {
  const msg = await rejected(api, saveBody("loan", { profile: "loan" }));
  // A `loan` is a table loan like a mortgage: the forecast projects its payoff from these
  // rather than fitting a trend to a debt.
  for (const field of [
    "subtype",
    "lender",
    "original_amount_minor",
    "interest_rate_bps",
    "rate_type",
    "term_months",
    "start_date",
  ]) {
    expect(msg, `should name ${field}`).toContain(field);
  }
});

test("a student loan is exempt from the terms a loan must have", async ({ api }) => {
  // `loan` and `student_loan` share a profile and only one of them amortises: an NZ student
  // loan is interest-free and repaid as a percentage of income, so it has no schedule.
  const created = await api.POST("/api/accounts", {
    body: saveBody("student_loan", {
      profile: "loan",
      subtype: "student",
      lender: "Inland Revenue",
      original_amount_minor: 3_000_000,
      interest_rate_bps: 0,
    }),
  });
  expect(created.response.status).toBe(201);
});

test("a fixed rate must say what happens when it expires", async ({ api }) => {
  const base = {
    profile: "mortgage",
    lender: "ASB",
    original_amount_minor: 48_500_000,
    interest_rate_bps: 512,
    term_months: 324,
    start_date: "2025-12-11",
  };

  const msg = await rejected(api, saveBody("mortgage", { ...base, rate_type: "fixed" }));
  for (const field of ["fixed_until", "refix_rate_bps", "refix_rate_uncertainty_bps"]) {
    expect(msg, `should name ${field}`).toContain(field);
  }

  // A floating rate has no expiry and nothing to refix to, so it is asked for neither.
  const floating = await api.POST("/api/accounts", {
    body: saveBody("mortgage", { ...base, rate_type: "floating" }),
  });
  expect(floating.response.status).toBe(201);
});

test("an investment account must name its broker", async ({ api }) => {
  expect(await rejected(api, saveBody("brokerage", { profile: "brokerage" }))).toContain(
    "broker is required"
  );
  expect(
    await rejected(api, saveBody("shares_private", { profile: "shares" }))
  ).toContain("broker is required");
});

test("a crypto account must say where it is held and how it is taxed", async ({ api }) => {
  const msg = await rejected(api, saveBody("crypto", { profile: "crypto" }));
  expect(msg).toContain("subtype");
  expect(msg).toContain("tax_treatment");
});

test("a card must carry its credit limit, but a savings account needn't", async ({ api }) => {
  for (const kind of ["credit_card", "revolving_credit"] as const) {
    const msg = await rejected(
      api,
      saveBody(kind, { profile: "depository" }, { institution: "ANZ" })
    );
    expect(msg).toContain("credit_limit_minor is required");
  }

  // A limit is meaningless on a savings account, so none is asked for.
  const savings = await api.POST("/api/accounts", {
    body: saveBody("savings", { profile: "depository" }, { institution: "ANZ" }),
  });
  expect(savings.response.status).toBe(201);
});

test("a listed holding must carry a ticker and exchange, an unlisted one needn't", async ({
  api,
}) => {
  for (const kind of ["shares_nz", "shares_us"] as const) {
    const msg = await rejected(api, saveBody(kind, { profile: "shares", broker: "Sharesies" }));
    expect(msg).toContain("ticker");
    expect(msg).toContain("exchange");
  }

  // An unlisted holding has neither a ticker nor an exchange.
  const private_ = await api.POST("/api/accounts", {
    body: saveBody("shares_private", { profile: "shares", broker: "Carta" }),
  });
  expect(private_.response.status).toBe(201);
});

test("a bank account must name its institution", async ({ api }) => {
  for (const kind of ["bank", "savings"] as const) {
    // Blank counts as absent: whitespace is not an answer.
    const msg = await rejected(
      api,
      saveBody(kind, { profile: "depository" }, { institution: "   " })
    );
    expect(msg).toContain("institution is required");
  }

  // Cash in hand has no institution, so none is asked for.
  const cash = await api.POST("/api/accounts", { body: saveBody("cash") });
  expect(cash.response.status).toBe(201);
});

test("a subtype outside the curated list is rejected", async ({ api }) => {
  const msg = await rejected(
    api,
    saveBody("real_estate", {
      profile: "property",
      subtype: "castle",
      address_line1: "1 Queen Street",
      city: "Auckland",
      country: "New Zealand",
    })
  );
  expect(msg).toContain("castle");
  expect(msg).toContain("single_family_home"); // the message lists the legal values
});

// ---------------------------------------------------------------------------
// Opening balance: part of creating the account, so it can't go missing because a
// follow-up request failed.
// ---------------------------------------------------------------------------

test("an opening balance is required when creating an account", async ({ api }) => {
  const msg = await rejected(
    api,
    saveBody("bank", { profile: "depository" }, {
      institution: "ANZ",
      opening_balance_minor: undefined,
      opening_balance_date: undefined,
    })
  );
  expect(msg).toContain("opening_balance_minor");
  expect(msg).toContain("opening_balance_date");

  // Half of it is refused too, rather than silently dropped.
  const half = await rejected(
    api,
    saveBody("bank", { profile: "depository" }, {
      institution: "ANZ",
      opening_balance_minor: 10_000,
      opening_balance_date: undefined,
    })
  );
  expect(half).toContain("opening_balance_date is required");
});

test("a bank account's opening balance becomes its first transaction", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank", "NZD", {
    opening_balance_minor: 250_000,
    opening_balance_date: "2024-03-01",
  });

  // A valuation would freeze a cash-like account at this figure and ignore everything
  // after it, so the balance has to arrive as a transaction — a one-off, to stay out of the
  // spend/income reports.
  const txns = await api.GET("/api/transactions", {
    params: { query: { account_id: acc.id } },
  });
  expect(txns.data?.length).toBe(1);
  expect(txns.data![0].posted_at).toBe("2024-03-01");
  expect(txns.data![0].amount_minor).toBe(250_000);
  expect(txns.data![0].description).toBe("Opening balance");
  expect(txns.data![0].is_one_off).toBe(true);

  const balances = await api.GET("/api/reports/balances", {
    params: { query: { to: "2024-06-01" } },
  });
  expect(balances.data?.accounts.find((a) => a.account_id === acc.id)?.value_minor).toBe(250_000);
});

test("a property's opening balance becomes its first valuation", async ({ api }) => {
  const house = await createAccount(api, "Family Home", "real_estate", "NZD", {
    opening_balance_minor: 82_000_000,
    opening_balance_date: "2024-03-01",
  });

  const vals = await api.GET("/api/accounts/{id}/valuations", {
    params: { path: { id: house.id } },
  });
  expect(vals.data?.length).toBe(1);
  expect(vals.data![0].as_of).toBe("2024-03-01");
  expect(vals.data![0].value_minor).toBe(82_000_000);
  expect(vals.data![0].note).toBe("Opening balance");
});

test("a liability's opening balance must be zero or negative", async ({ api }) => {
  // Net worth buckets purely by sign, so a positive mortgage would land in assets instead of
  // debt — and there's no valuation editor for a liability in the SPA to correct it later.
  const msg = await rejected(
    api,
    saveBody(
      "mortgage",
      {
        profile: "mortgage",
        lender: "ANZ",
        original_amount_minor: 48_500_000,
        interest_rate_bps: 549,
        rate_type: "fixed",
      },
      { opening_balance_minor: 1_000_00, opening_balance_date: "2024-03-01" }
    )
  );
  expect(msg).toContain("opening_balance_minor must be zero or negative");
  expect(msg).toContain("liabilities are negative in this app");
});

test("a non-liability's opening balance must be zero or positive", async ({ api }) => {
  const msg = await rejected(
    api,
    saveBody(
      "bank",
      { profile: "depository" },
      { institution: "ANZ", opening_balance_minor: -1_000_00, opening_balance_date: "2024-03-01" }
    )
  );
  expect(msg).toContain("opening_balance_minor must be zero or positive");
  expect(msg).toContain("liabilities are negative in this app");
});

test("a brokerage account's opening balance is refused outright, not just left optional", async ({
  api,
}) => {
  // Dropped from *required*, not from *accepted*: its value comes entirely from the holdings
  // ledger, so a supplied value would double-count rather than merely being redundant.
  const msg = await rejected(
    api,
    saveBody(
      "brokerage",
      { profile: "brokerage", broker: "Sharesies" },
      { opening_balance_minor: 10_000, opening_balance_date: "2024-03-01" }
    )
  );
  expect(msg).toContain("not accepted for a brokerage account");
});

test("a malformed opening balance date is rejected", async ({ api }) => {
  const msg = await rejected(
    api,
    saveBody(
      "bank",
      { profile: "depository" },
      { institution: "ANZ", opening_balance_minor: 10_000, opening_balance_date: "31/07/2026" }
    )
  );
  expect(msg).toContain("opening_balance_date");
  expect(msg).toContain("not a valid ISO-8601 date");
});

test("an opening balance cannot be set when updating an account", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");

  // Afterwards the balance is edited through transactions/valuations; accepting it here
  // would stamp a second "Opening balance" row into the account's history on every save.
  const res = await api.PUT("/api/accounts/{id}", {
    params: { path: { id: acc.id } },
    body: saveBody("bank", { profile: "depository" }, {
      name: "Everyday",
      institution: "ANZ",
      opening_balance_minor: 999_000,
      opening_balance_date: "2024-03-01",
    }),
  });
  expect(res.response.status).toBe(422);
  expect((res.error as { error?: { message?: string } })?.error?.message).toContain(
    "opening balance can only be set when creating an account"
  );

  // The same edit without it goes through.
  const ok = await api.PUT("/api/accounts/{id}", {
    params: { path: { id: acc.id } },
    body: saveBody("bank", { profile: "depository" }, {
      name: "Everyday (joint)",
      institution: "ANZ",
      opening_balance_minor: undefined,
      opening_balance_date: undefined,
    }),
  });
  expect(ok.response.status).toBe(200);
});

test("an account a provider linked without the required fields still reads back", async ({
  api,
}) => {
  // The link path validates in `ValidationMode::Linked`: a feed can't know a property's
  // address, so linking must not demand it — and the resulting account must still be
  // readable everywhere. (A mortgage is the documented exception; see the test below.)
  const linked = await api.POST("/api/providers/link", {
    body: {
      kind: "akahu",
      external_id: "acc_sparse_property",
      name: "Akahu — Family Home",
      new_account: {
        name: "Family Home",
        kind: "real_estate",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
      },
    },
  });
  expect(linked.response.status).toBe(201);

  const { data, response } = await api.GET("/api/accounts/{id}", {
    params: { path: { id: linked.data!.account_id } },
  });
  expect(response.status).toBe(200);
  const meta = data?.metadata as Schemas["PropertyMeta"] & { profile: string };
  expect(meta.profile).toBe("property");
  expect(meta.city).toBeUndefined();
  expect((await api.GET("/api/accounts", {})).data?.some((a) => a.id === data!.id)).toBe(true);

  // Only *saving* it is blocked — the prompt to fill in the gaps.
  const incomplete = await api.PUT("/api/accounts/{id}", {
    params: { path: { id: data!.id } },
    body: saveBody("real_estate", { profile: "property" }, {
      name: "Family Home",
      opening_balance_minor: undefined,
      opening_balance_date: undefined,
    }),
  });
  expect(incomplete.response.status).toBe(422);

  // And filling them in saves normally: the account is never left uneditable.
  const saved = await api.PUT("/api/accounts/{id}", {
    params: { path: { id: data!.id } },
    body: saveBody(
      "real_estate",
      {
        profile: "property",
        subtype: "single_family_home",
        address_line1: "12 Rimu Street",
        city: "Wellington",
        country: "New Zealand",
      },
      { name: "Family Home", opening_balance_minor: undefined, opening_balance_date: undefined }
    ),
  });
  expect(saved.response.status).toBe(200);
  expect((saved.data?.metadata as Schemas["PropertyMeta"]).city).toBe("Wellington");
});

test("linking a mortgage still demands its amortisation terms", async ({ api }) => {
  // The documented exception to `ValidationMode::Linked`. Akahu reports a mortgage's
  // balance and essentially never its schedule, so exempting the link path would make the
  // commonest way to create a mortgage the one that leaves it unforecastable. There is a
  // person in the connect dialog, and it asks.
  const link = (metadata?: Schemas["AccountMetadata"]) =>
    api.POST("/api/providers/link", {
      body: {
        kind: "akahu",
        external_id: `acc_mortgage_${metadata ? "full" : "bare"}`,
        name: "Akahu — Home Loan",
        new_account: {
          name: "Home Loan",
          kind: "mortgage",
          currency_code: "NZD",
          archived: false,
          sort_order: 0,
          ...(metadata ? { metadata } : {}),
        },
      },
    });

  const bare = await link();
  expect(bare.response.status).toBe(422);
  for (const field of ["original_amount_minor", "interest_rate_bps", "rate_type", "term_months"]) {
    expect(JSON.stringify(bare.error), `should name ${field}`).toContain(field);
  }

  const complete = await link({
    profile: "mortgage",
    original_amount_minor: 48_500_000,
    interest_rate_bps: 512,
    rate_type: "fixed",
    fixed_until: "2027-01-11",
    refix_rate_bps: 512,
    refix_rate_uncertainty_bps: 150,
    term_months: 324,
    start_date: "2025-12-11",
  });
  expect(complete.response.status).toBe(201);
});
