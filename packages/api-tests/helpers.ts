import net from "node:net";
import { expect } from "@playwright/test";
import { deflateRawSync } from "node:zlib";
import type { Schemas, SureClient } from "../client/src/index";

type AccountKind = Schemas["SaveAccount"]["kind"];
type CategoryKind = NonNullable<Schemas["SaveCategory"]["kind"]>;

/**
 * The metadata each kind now *requires* — see `AccountMetadata::validate_for` in sure-core.
 * Kinds absent here need none: our `kind` already says what a depository subtype would, and
 * "other asset"/"other liability" are deliberately free-form.
 *
 * Values are only there to be valid; a spec that cares about a field passes its own, which
 * wins over these (see {@link createAccount}).
 */
const REQUIRED_METADATA: Partial<Record<AccountKind, Record<string, unknown>>> = {
  real_estate: {
    profile: "property",
    subtype: "single_family_home",
    address_line1: "12 Rimu Street",
    city: "Wellington",
    country: "New Zealand",
  },
  vehicle: { profile: "vehicle", make: "Toyota", model: "RAV4", year: 2021 },
  // A mortgage/loan carries its whole amortisation schedule: term and start date make the
  // payoff projectable, and a fixed rate must also say what to assume once it expires.
  mortgage: {
    profile: "mortgage",
    lender: "ASB",
    original_amount_minor: 48_500_000,
    interest_rate_bps: 549,
    rate_type: "fixed",
    fixed_until: "2027-01-11",
    refix_rate_bps: 549,
    refix_rate_uncertainty_bps: 150,
    term_months: 360,
    start_date: "2024-01-01",
  },
  // Floating, so it needs no refix terms — the fixed case is covered by `mortgage`.
  loan: {
    profile: "loan",
    subtype: "other",
    lender: "MTF Finance",
    original_amount_minor: 1_500_000,
    interest_rate_bps: 890,
    rate_type: "floating",
    term_months: 60,
    start_date: "2024-01-01",
  },
  // Its own profile, and the whole of it: an income-contingent loan has no principal, term or
  // schedule to state. Interest-free because the borrower is NZ-based, which is a real answer
  // rather than a stand-in for one.
  student_loan: { profile: "student_loan", lender: "Inland Revenue", interest_rate_bps: 0 },
  // Only the revolving kinds carry a limit.
  credit_card: { profile: "depository", credit_limit_minor: 1_000_000 },
  revolving_credit: { profile: "depository", credit_limit_minor: 5_000_000 },
  // A listed holding is priced by (ticker, exchange); an unlisted one has neither.
  shares_nz: { profile: "shares", broker: "Sharesies", ticker: "MEL", exchange: "NZX" },
  shares_us: { profile: "shares", broker: "Sharesies", ticker: "VOO", exchange: "NYSE Arca" },
  shares_private: { profile: "shares", broker: "Carta" },
  brokerage: { profile: "brokerage", broker: "Sharesies" },
  crypto: { profile: "crypto", subtype: "wallet", tax_treatment: "taxable" },
};

/** The kinds whose account-level institution is required. */
const INSTITUTION_REQUIRED: AccountKind[] = ["bank", "savings", "credit_card", "revolving_credit"];

/**
 * Create an account, asserting success, and return it.
 *
 * Saving an account means answering for its kind's identifying fields — and, on create, its
 * opening balance. Rather than have every spec restate a lender or broker it doesn't care
 * about, this fills in the required set per kind ({@link REQUIRED_METADATA}) underneath
 * whatever the caller passes, so a spec asserting on metadata still gets exactly what it
 * asked for. The opening balance defaults to zero, which deliberately seeds no ledger row,
 * leaving each spec's own transactions/valuations the only ones present.
 *
 * `ownership` is required by the API — every account belongs to a household member or is
 * joint. Specs that aren't about attribution get `joint`, which needs no person to exist;
 * the ones that are pass their own.
 */
export async function createAccount(
  api: SureClient,
  name: string,
  kind: AccountKind,
  currency = "NZD",
  extra: {
    metadata?: Schemas["AccountMetadata"];
    institution?: string;
    ownership?: Schemas["Ownership"];
    opening_balance_minor?: number;
    opening_balance_date?: string;
  } = {}
) {
  const { metadata, institution, ownership, ...openingBalance } = extra;
  // A brokerage account is the one kind with no opening balance: its value comes from the
  // holdings ledger.
  const openingBalanceDefaults =
    kind === "brokerage" ? {} : { opening_balance_minor: 0, opening_balance_date: "2020-01-01" };
  const { data, response } = await api.POST("/api/accounts", {
    body: {
      name,
      kind,
      currency_code: currency,
      archived: false,
      sort_order: 0,
      institution: institution ?? (INSTITUTION_REQUIRED.includes(kind) ? "ANZ" : undefined),
      ownership: ownership ?? { kind: "joint" },
      metadata: withRequiredMetadata(kind, metadata),
      ...openingBalanceDefaults,
      ...openingBalance,
    },
  });
  expect(response.status, "create account").toBe(201);
  return data!;
}

function withRequiredMetadata(kind: AccountKind, provided?: Schemas["AccountMetadata"]) {
  const required = REQUIRED_METADATA[kind];
  if (!required) return provided;
  const merged: Record<string, unknown> = { ...required, ...provided };
  // `address` is the legacy alias of `address_line1` — the same serde field, so a spec
  // writing the old key must not also be handed the new one (serde rejects both at once).
  if (provided && "address" in provided) delete merged.address_line1;
  return merged as Schemas["AccountMetadata"];
}

export async function createCategory(
  api: SureClient,
  name: string,
  kind: CategoryKind = "expense",
  parentId: number | null = null
) {
  const { data, response } = await api.POST("/api/categories", {
    body: { name, kind, parent_id: parentId, sort_order: 0 },
  });
  expect(response.status, "create category").toBe(201);
  return data!;
}

export async function createMerchant(api: SureClient, name: string, categoryId: number | null = null) {
  const { data, response } = await api.POST("/api/merchants", {
    body: { name, category_id: categoryId },
  });
  expect(response.status, "create merchant").toBe(201);
  return data!;
}

export async function createTransaction(
  api: SureClient,
  input: {
    account_id: number;
    posted_at: string;
    amount_minor: number;
    description?: string;
    category_id?: number | null;
    merchant_id?: number | null;
    exclude_from_cashflow?: boolean;
    /** Attribution override; omitted means "follow the account's owner", as an import does. */
    ownership?: Schemas["Ownership"];
    /** The account on the other side, where it is one — a mortgage, the house a deposit bought. */
    counterparty_account_id?: number | null;
  }
) {
  const { data, response } = await api.POST("/api/transactions", {
    body: {
      account_id: input.account_id,
      posted_at: input.posted_at,
      amount_minor: input.amount_minor,
      description: input.description ?? "x",
      category_id: input.category_id ?? null,
      merchant_id: input.merchant_id ?? null,
      exclude_from_cashflow: input.exclude_from_cashflow ?? false,
      ownership: input.ownership ?? null,
      counterparty_account_id: input.counterparty_account_id ?? null,
    },
  });
  expect(response.status, "create transaction").toBe(201);
  return data!;
}

export async function getTransaction(api: SureClient, id: number) {
  const { data, response } = await api.GET("/api/transactions/{id}", {
    params: { path: { id } },
  });
  expect(response.status).toBe(200);
  return data!;
}

// ---- a tiny zip builder (no dependency; the Rust `zip` reader accepts it) ---------------
//
// STORE by default. `{ deflate: true }` switches to method 8, which a test needs to build
// something small on the wire that expands enormously — a stored entry can't do that.

export function crc32(buf: Buffer): number {
  let crc = 0xffffffff;
  for (let i = 0; i < buf.length; i++) {
    let c = (crc ^ buf[i]) & 0xff;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    crc = (crc >>> 8) ^ c;
  }
  return (crc ^ 0xffffffff) >>> 0;
}

export function makeZip(
  files: Record<string, string | Uint8Array>,
  opts: { deflate?: boolean } = {}
): ArrayBuffer {
  const method = opts.deflate ? 8 : 0;
  const localParts: Buffer[] = [];
  const central: Buffer[] = [];
  let offset = 0;
  for (const [name, content] of Object.entries(files)) {
    const nameBuf = Buffer.from(name, "utf8");
    const data = typeof content === "string" ? Buffer.from(content, "utf8") : Buffer.from(content);
    // The CRC and the "uncompressed size" field both describe the original bytes; only the
    // stored payload and its size change when deflating.
    const crc = crc32(data);
    const payload = opts.deflate ? deflateRawSync(data) : data;

    const local = Buffer.alloc(30 + nameBuf.length);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(method, 8);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(payload.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    nameBuf.copy(local, 30);
    localParts.push(local, payload);

    const cd = Buffer.alloc(46 + nameBuf.length);
    cd.writeUInt32LE(0x02014b50, 0);
    cd.writeUInt16LE(20, 4);
    cd.writeUInt16LE(20, 6);
    cd.writeUInt16LE(method, 10);
    cd.writeUInt32LE(crc, 16);
    cd.writeUInt32LE(payload.length, 20);
    cd.writeUInt32LE(data.length, 24);
    cd.writeUInt16LE(nameBuf.length, 28);
    cd.writeUInt32LE(offset, 42);
    nameBuf.copy(cd, 46);
    central.push(cd);

    offset += local.length + payload.length;
  }
  const cdBuf = Buffer.concat(central);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0);
  eocd.writeUInt16LE(central.length, 8);
  eocd.writeUInt16LE(central.length, 10);
  eocd.writeUInt32LE(cdBuf.length, 12);
  eocd.writeUInt32LE(offset, 16);
  const full = Buffer.concat([...localParts, cdBuf, eocd]);
  // Copy into a freshly-allocated ArrayBuffer so the type is a plain `ArrayBuffer` (a
  // valid `BodyInit`) rather than Node's `Buffer<ArrayBufferLike>`, which TS rejects.
  const ab = new ArrayBuffer(full.byteLength);
  new Uint8Array(ab).set(full);
  return ab;
}

// ---- oversized bodies -------------------------------------------------------------------

/**
 * The server's default body caps, as `sure_api::config::Limits::default` sets them. Every
 * backend this suite spawns runs on the defaults, so these are what [`postOversized`] has to
 * exceed, by exactly one byte.
 */
export const BODY_LIMITS = {
  /** Every route without a cap of its own (`MAX_BODY_BYTES`). */
  global: 2 * 1024 * 1024,
  /** `POST /api/import`, every file upload (`MAX_IMPORT_BODY_BYTES`, `sure_core::MAX_UPLOAD_BYTES`). */
  upload: 16 * 1024 * 1024,
  /** `POST /api/config/import` (`MAX_SNAPSHOT_BODY_BYTES`). */
  snapshot: 32 * 1024 * 1024,
} as const;

/** Which endpoint an oversized-body probe should hit. */
export type OversizedTarget = { path?: string; contentType?: string };

/**
 * POST a body one byte over `limit` and return the server's refusal.
 *
 * Deliberately not `fetch`, and deliberately one byte over. The cap is enforced as the body is
 * read: the server answers 413 and closes as soon as it has read past the limit. Whatever the
 * client sent beyond that point is still unread in the server's receive buffer, which makes the
 * close an RST rather than a FIN, and an RST tells the client's kernel to discard what it has
 * buffered, the 413 included. `undici` reports that as `TypeError: fetch failed`. A raw socket
 * only wins the race when it reads first, and under the load of a full suite run it lost five
 * times in a row (a 51 MB body against the 16 MiB upload cap), which failed commits.
 *
 * One byte over is a body the server has to read to the end before it can tell it is too big, so
 * nothing is left unread, the close is a FIN, and the response survives however late it is read.
 * `Connection: close` makes the server close after answering rather than wait for another
 * request on a connection whose body it has finished.
 *
 * So a reset here is never the race: it means the server refused before the end of the body,
 * i.e. `limit` is no longer its cap, and the probe fails saying so.
 */
export function postOversized(
  baseURL: string,
  limit: number,
  { path = "/api/accounts", contentType = "application/json" }: OversizedTarget = {},
): Promise<Response> {
  const url = new URL(baseURL);
  const bytes = limit + 1;
  return new Promise((resolve, reject) => {
    const socket = net.createConnection({ host: url.hostname, port: Number(url.port) });
    const chunks: Buffer[] = [];
    let reset: Error | undefined;
    socket.on("error", (err) => (reset = err));
    socket.on("data", (chunk) => chunks.push(chunk));
    socket.on("close", () => {
      if (reset) {
        return reject(
          new Error(
            `the server reset the connection (${reset.message}), so it refused ${path} before ` +
              `the end of a ${bytes}-byte body: ${limit} is no longer its cap`,
          ),
        );
      }
      const raw = Buffer.concat(chunks).toString("latin1");
      const separator = raw.indexOf("\r\n\r\n");
      if (separator === -1) {
        return reject(new Error(`the server closed ${path} without a complete response`));
      }
      const [statusLine, ...headerLines] = raw.slice(0, separator).split("\r\n");
      const headers = new Headers(
        headerLines.map((line) => {
          const at = line.indexOf(":");
          return [line.slice(0, at), line.slice(at + 1).trim()] as [string, string];
        }),
      );
      // Rebuilt as a `Response` so the assertions below read like every other test here.
      resolve(
        new Response(raw.slice(separator + 4), {
          status: Number(statusLine.split(" ")[1]),
          headers,
        }),
      );
    });
    socket.on("connect", () => {
      socket.write(
        `POST ${path} HTTP/1.1\r\nHost: ${url.host}\r\nConnection: close\r\n` +
          `Content-Type: ${contentType}\r\nContent-Length: ${bytes}\r\n\r\n`,
      );
      // Written in chunks, and only while the socket is still up: one 3 MB `write` would
      // sit in Node's buffer and keep the process alive after the peer had gone.
      const chunk = Buffer.alloc(64 * 1024, "a");
      let sent = 0;
      const pump = () => {
        while (sent < bytes && !socket.destroyed && !socket.writableEnded) {
          const size = Math.min(chunk.length, bytes - sent);
          sent += size;
          if (!socket.write(chunk.subarray(0, size))) return socket.once("drain", pump);
        }
      };
      pump();
    });
  });
}

/**
 * Someone in the household. Open-coded in three specs before this existed, each slightly
 * differently.
 */
export async function createPerson(api: SureClient, name: string, color?: string) {
  const { data, response } = await api.POST("/api/people", {
    body: { name, color, sort_order: 0 },
  });
  expect(response.status, "create person").toBe(201);
  return data!;
}
