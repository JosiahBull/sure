import { test, expect } from "../fixtures";
import type { Schemas } from "../../client/src/index";
import { createAccount } from "../helpers";

test("property metadata round-trips with typed fields", async ({ api }) => {
  const house = await createAccount(api, "Family Home", "real_estate", "NZD", {
    metadata: {
      profile: "property",
      address: "14 Kōwhai Street, Wellington",
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
  expect(meta.address).toBe("14 Kōwhai Street, Wellington");
  expect(meta.purchase_date).toBe("2023-05-15");
  expect(meta.purchase_price_minor).toBe(74_000_000);
  expect(meta.url).toBe("https://www.qv.co.nz");
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

test("a credit card's credit limit round-trips and drives remaining-borrowing", async ({ api }) => {
  const card = await createAccount(api, "Visa Platinum", "credit_card", "NZD", {
    metadata: { profile: "depository", account_number: "••1234", credit_limit_minor: 1_000_000 },
  });
  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: card.id } } });
  const meta = data?.metadata as Schemas["DepositoryMeta"] & { profile: string };
  expect(meta.credit_limit_minor).toBe(1_000_000);
  expect(meta.account_number).toBe("••1234");
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

test("an account created without metadata gets an empty value for its kind", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const { data } = await api.GET("/api/accounts/{id}", { params: { path: { id: acc.id } } });
  const meta = data?.metadata as { profile: string };
  expect(meta.profile).toBe("depository");
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
      metadata: { profile: "vehicle", make: "Toyota", model: "Corolla", nickname: "new name" },
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
